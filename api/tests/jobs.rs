use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::{MemoryJobStore, MemoryStore};
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use fhir_store::{JobId, JobStore, StepTicker};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

fn service(jobs: Arc<dyn JobStore>) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Ok(())),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies).with_jobs(jobs)
}

async fn request(app: &Service, method: &str, uri: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or_default().to_owned()))
        .collect();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn header<'a>(reply: &'a Reply, name: &str) -> &'a str {
    reply
        .headers
        .iter()
        .find(|(held, _)| held.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn queue() -> (Arc<MemoryJobStore>, StepTicker) {
    let ticker = StepTicker::starting_at(1_000);
    (Arc::new(MemoryJobStore::new(ticker.ticker())), ticker)
}

fn submitted_id(reply: &Reply) -> JobId {
    let location = header(reply, "content-location");
    let tail = location.rsplit('/').next().expect("a status location ends in an id");
    JobId::parse(tail).expect("the announced id is valid")
}

#[tokio::test]
async fn a_submission_answers_with_a_status_location_and_a_retry_after() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(&app, "POST", "/$export", br#"{"types":["Patient"]}"#).await;

    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let location = header(&reply, "content-location");
    assert!(location.contains("/_jobs/"), "{location}");
    assert!(!header(&reply, "retry-after").is_empty());
    let held = jobs.fetch(&submitted_id(&reply)).await.unwrap();
    assert_eq!(held.kind, fhir_store::JobKind::Export);
    assert_eq!(held.payload.as_deref(), Some(r#"{"types":["Patient"]}"#));
}

#[tokio::test]
async fn every_job_operation_submits_its_own_kind() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let cases = [
        ("/$export", fhir_store::JobKind::Export),
        ("/$import", fhir_store::JobKind::Import),
        ("/$bulk-delete", fhir_store::JobKind::BulkDelete),
        ("/$bulk-update", fhir_store::JobKind::BulkUpdate),
        ("/$reindex", fhir_store::JobKind::Reindex),
    ];
    for (path, kind) in cases {
        let reply = request(&app, "POST", path, b"{}").await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{path}");
        let held = jobs.fetch(&submitted_id(&reply)).await.unwrap();
        assert_eq!(held.kind, kind, "{path}");
    }
}

#[tokio::test]
async fn polling_a_queued_job_reports_progress_and_asks_to_wait() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = request(&app, "POST", "/$export", b"{}").await;
    let id = submitted_id(&reply);

    let polled = request(&app, "GET", &format!("/_jobs/{id}"), b"").await;

    assert_eq!(polled.status, StatusCode::ACCEPTED);
    assert!(!header(&polled, "retry-after").is_empty());
    assert!(header(&polled, "x-progress").contains("queued"), "{:?}", polled.headers);
}

#[tokio::test]
async fn polling_a_finished_job_answers_with_its_manifest() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = request(&app, "POST", "/$export", b"{}").await;
    let id = submitted_id(&reply);
    jobs.claim(&fhir_store::Lease::new("one", 1_000)).await.unwrap();
    jobs.finish(&id, "one", fhir_store::JobResult::Succeeded("{\"handled\":2}".to_owned()))
        .await
        .unwrap();

    let polled = request(&app, "GET", &format!("/_jobs/{id}"), b"").await;

    assert_eq!(polled.status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&polled.body).unwrap();
    assert_eq!(body["id"], id.as_str());
    assert_eq!(body["kind"], "export");
    assert_eq!(body["state"], "completed");
    assert_eq!(body["outcome"]["handled"], 2);
}

#[tokio::test]
async fn polling_a_failed_job_answers_with_an_outcome() {
    let (jobs, ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = request(&app, "POST", "/$export", b"{}").await;
    let id = submitted_id(&reply);
    while jobs.fetch(&id).await.unwrap().state != fhir_store::JobState::Failed {
        ticker.advance(10_000);
        jobs.claim(&fhir_store::Lease::new("one", 1_000)).await.unwrap();
        jobs.finish(&id, "one", fhir_store::JobResult::Failed("no store".to_owned()))
            .await
            .unwrap();
    }

    let polled = request(&app, "GET", &format!("/_jobs/{id}"), b"").await;

    assert_eq!(polled.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(polled.body.contains("OperationOutcome"), "{}", polled.body);
}

#[tokio::test]
async fn polling_an_unknown_job_is_not_found() {
    let (jobs, _ticker) = queue();
    let app = service(jobs as Arc<dyn JobStore>);

    let polled = request(&app, "GET", "/_jobs/nobody", b"").await;

    assert_eq!(polled.status, StatusCode::NOT_FOUND);
    assert!(polled.body.contains("OperationOutcome"));
}

#[tokio::test]
async fn cancelling_a_queued_job_stops_it_and_polling_then_reports_it_gone() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = request(&app, "POST", "/$export", b"{}").await;
    let id = submitted_id(&reply);

    let cancelled = request(&app, "DELETE", &format!("/_jobs/{id}"), b"").await;

    assert_eq!(cancelled.status, StatusCode::ACCEPTED);
    assert_eq!(
        jobs.fetch(&id).await.unwrap().state,
        fhir_store::JobState::Cancelled
    );
    let polled = request(&app, "GET", &format!("/_jobs/{id}"), b"").await;
    assert_eq!(polled.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancelling_a_finished_job_conflicts() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = request(&app, "POST", "/$export", b"{}").await;
    let id = submitted_id(&reply);
    jobs.claim(&fhir_store::Lease::new("one", 1_000)).await.unwrap();
    jobs.finish(&id, "one", fhir_store::JobResult::Succeeded("{}".to_owned()))
        .await
        .unwrap();

    let cancelled = request(&app, "DELETE", &format!("/_jobs/{id}"), b"").await;

    assert_eq!(cancelled.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_service_without_a_queue_refuses_a_submission() {
    let store = MemoryStore::default();
    let app = Service::new(Arc::new(store), FhirVersion::R4, Vec::new());

    let reply = request(&app, "POST", "/$export", b"{}").await;

    assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
}
