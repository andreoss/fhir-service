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
        check: Arc::new(|| Box::pin(async { Ok(()) })),
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
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
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
    let tail = location
        .rsplit('/')
        .next()
        .expect("a status location ends in an id");
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
    let payload: serde_json::Value = serde_json::from_str(
        held.payload
            .as_deref()
            .expect("a job carries a description"),
    )
    .unwrap();
    assert_eq!(payload["types"][0], "Patient");
    assert_eq!(payload["scope"], "system");
    assert!(payload["_till"].is_string());
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
    assert!(
        header(&polled, "x-progress").contains("queued"),
        "{:?}",
        polled.headers
    );
}

#[tokio::test]
async fn polling_a_finished_job_answers_with_its_manifest() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = request(&app, "POST", "/$export", b"{}").await;
    let id = submitted_id(&reply);
    jobs.claim(&fhir_store::Lease::new("one", 1_000))
        .await
        .unwrap();
    jobs.finish(
        &id,
        "one",
        fhir_store::JobResult::Succeeded("{\"handled\":2}".to_owned()),
    )
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
        jobs.claim(&fhir_store::Lease::new("one", 1_000))
            .await
            .unwrap();
        jobs.finish(
            &id,
            "one",
            fhir_store::JobResult::Failed("no store".to_owned()),
        )
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
    jobs.claim(&fhir_store::Lease::new("one", 1_000))
        .await
        .unwrap();
    jobs.finish(
        &id,
        "one",
        fhir_store::JobResult::Succeeded("{}".to_owned()),
    )
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

fn payload_of(record: &fhir_store::JobRecord) -> serde_json::Value {
    serde_json::from_str(record.payload.as_deref().expect("a job carries a payload"))
        .expect("the payload is an object")
}

#[tokio::test]
async fn a_type_scoped_bulk_delete_carries_the_type_it_was_asked_under() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(
        &app,
        "POST",
        "/Patient/$bulk-delete?hardDelete=true&_maxCount=5",
        b"",
    )
    .await;

    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let record = jobs.fetch(&submitted_id(&reply)).await.unwrap();
    assert_eq!(record.kind, fhir_store::JobKind::BulkDelete);
    let payload = payload_of(&record);
    assert_eq!(payload["_type"][0], "Patient");
    assert_eq!(payload["hardDelete"], "true");
    assert_eq!(payload["_maxCount"], "5");
    assert_eq!(payload["softDeleted"], false);
}

#[tokio::test]
async fn a_delete_of_the_soft_deleted_is_marked_as_such() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(&app, "POST", "/Patient/$bulk-delete-soft-deleted", b"").await;

    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let payload = payload_of(&jobs.fetch(&submitted_id(&reply)).await.unwrap());
    assert_eq!(payload["softDeleted"], true);
    assert_eq!(payload["_type"][0], "Patient");

    let system = request(&app, "POST", "/$bulk-delete-soft-deleted", b"").await;
    let carried = payload_of(&jobs.fetch(&submitted_id(&system)).await.unwrap());
    assert_eq!(carried["softDeleted"], true);
    assert_eq!(carried["_type"], serde_json::Value::Null);
}

#[tokio::test]
async fn excluded_types_are_carried_as_a_list() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(
        &app,
        "POST",
        "/$bulk-delete?_exclude=Observation,Group",
        b"",
    )
    .await;

    let payload = payload_of(&jobs.fetch(&submitted_id(&reply)).await.unwrap());
    assert_eq!(payload["_exclude"][0], "Observation");
    assert_eq!(payload["_exclude"][1], "Group");
}

#[tokio::test]
async fn a_bulk_delete_refuses_a_parameter_it_does_not_know() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(&app, "POST", "/$bulk-delete?_wrong=1", b"").await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(reply.body.contains("OperationOutcome"), "{}", reply.body);
}

#[tokio::test]
async fn a_type_scoped_bulk_update_carries_the_patch_it_was_given() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(
        &app,
        "POST",
        "/Patient/$bulk-update?_maxCount=3",
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    )
    .await;

    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let record = jobs.fetch(&submitted_id(&reply)).await.unwrap();
    assert_eq!(record.kind, fhir_store::JobKind::BulkUpdate);
    let payload = payload_of(&record);
    assert_eq!(payload["_type"][0], "Patient");
    assert_eq!(payload["_maxCount"], "3");
    assert_eq!(payload["patch"][0]["op"], "replace");
}

#[tokio::test]
async fn a_bulk_update_described_by_an_object_keeps_its_own_patch() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(
        &app,
        "POST",
        "/$bulk-update?_exclude=Observation",
        br#"{"patch":[{"op":"add","path":"/language","value":"en"}]}"#,
    )
    .await;

    let payload = payload_of(&jobs.fetch(&submitted_id(&reply)).await.unwrap());
    assert_eq!(payload["patch"][0]["op"], "add");
    assert_eq!(payload["_exclude"][0], "Observation");
}

#[tokio::test]
async fn a_bulk_update_refuses_a_parameter_it_does_not_know() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(&app, "POST", "/$bulk-update?_since=2026", b"[]").await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_resource_reindex_names_the_resource_it_was_asked_under() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(&app, "POST", "/Patient/p1/$reindex", b"").await;

    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let record = jobs.fetch(&submitted_id(&reply)).await.unwrap();
    assert_eq!(record.kind, fhir_store::JobKind::Reindex);
    assert_eq!(payload_of(&record)["_resource"][0], "Patient/p1");
}

#[tokio::test]
async fn a_reindex_carries_the_parameters_it_was_targeted_at() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = request(&app, "POST", "/$reindex?_url=urn:p:band&_type=Patient", b"").await;

    let payload = payload_of(&jobs.fetch(&submitted_id(&reply)).await.unwrap());
    assert_eq!(payload["_url"][0], "urn:p:band");
    assert_eq!(payload["_type"][0], "Patient");
}
