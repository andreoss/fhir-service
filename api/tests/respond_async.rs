use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::{MemoryJobStore, MemoryStore};
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use fhir_jobs::{InteractionJob, Orchestrator};
use fhir_store::{JobId, JobKind, JobStore, Lease, StepTicker};
use http_body_util::BodyExt;
use serde_json::{json, Value};
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

async fn ask(app: &Service, method: &str, uri: &str, prefer: Option<&str>, body: &[u8]) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json");
    if let Some(prefer) = prefer {
        builder = builder.header("prefer", prefer);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
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

async fn run(app: &Service, jobs: &MemoryJobStore) {
    let claimed = jobs.claim(&Lease::new("one", 1_000)).await.unwrap();
    assert_eq!(claimed.len(), 1, "the deferred interaction was queued");
    let orchestrator = Orchestrator::new().with(Arc::new(InteractionJob::new(app.interactions())));
    orchestrator
        .run(jobs, &claimed[0], "one", 1_000)
        .await
        .unwrap();
}

fn patient(id: &str, family: &str) -> Vec<u8> {
    json!({
        "resourceType": "Patient",
        "id": id,
        "name": [{"family": family}],
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn a_search_asked_to_answer_later_is_accepted_with_a_status_location() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = ask(
        &app,
        "GET",
        "/Patient?name=River",
        Some("respond-async"),
        b"",
    )
    .await;

    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.body);
    let location = header(&reply, "content-location");
    assert!(location.contains("/_jobs/"), "{location}");
    assert!(!header(&reply, "retry-after").is_empty());
    let held = jobs.fetch(&submitted_id(&reply)).await.unwrap();
    assert_eq!(held.kind, JobKind::Interaction);
    let payload: Value = serde_json::from_str(held.payload.as_deref().unwrap()).unwrap();
    assert_eq!(payload["interaction"], "search");
    assert_eq!(payload["path"], "/Patient");
    assert_eq!(payload["type"], "Patient");
    assert_eq!(payload["query"], "name=River");
}

#[tokio::test]
async fn a_search_that_asks_for_nothing_is_answered_at_once() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = ask(&app, "GET", "/Patient", None, b"").await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(json(&reply.body)["type"], "searchset");
}

#[tokio::test]
async fn another_preference_does_not_defer_the_answer() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = ask(&app, "GET", "/Patient", Some("return=representation"), b"").await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(json(&reply.body)["type"], "searchset");
}

#[tokio::test]
async fn a_whole_system_search_asked_to_answer_later_is_accepted() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = ask(&app, "GET", "/?_type=Patient", Some("respond-async"), b"").await;

    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.body);
    let held = jobs.fetch(&submitted_id(&reply)).await.unwrap();
    assert_eq!(held.kind, JobKind::Interaction);
    let payload: Value = serde_json::from_str(held.payload.as_deref().unwrap()).unwrap();
    assert_eq!(payload["path"], "/");
    assert_eq!(payload["query"], "_type=Patient");
    assert!(payload.get("type").is_none());
}

#[tokio::test]
async fn a_deferred_interaction_that_is_still_pending_asks_to_wait() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = ask(&app, "GET", "/Patient", Some("respond-async"), b"").await;
    let id = submitted_id(&reply);

    let polled = ask(&app, "GET", &format!("/_jobs/{id}"), None, b"").await;

    assert_eq!(polled.status, StatusCode::ACCEPTED, "{}", polled.body);
    assert!(!header(&polled, "retry-after").is_empty());
    assert!(header(&polled, "x-progress").contains("queued"));
}

#[tokio::test]
async fn a_deferred_search_is_answered_with_a_batch_response_bundle() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let created = ask(&app, "POST", "/Patient", None, &patient("d1", "River")).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let reply = ask(
        &app,
        "GET",
        "/Patient?family=River",
        Some("respond-async"),
        b"",
    )
    .await;
    let id = submitted_id(&reply);
    run(&app, &jobs).await;

    let polled = ask(&app, "GET", &format!("/_jobs/{id}"), None, b"").await;

    assert_eq!(polled.status, StatusCode::OK, "{}", polled.body);
    let answered = json(&polled.body);
    assert_eq!(answered["resourceType"], "Bundle");
    assert_eq!(answered["type"], "batch-response");
    assert_eq!(answered["entry"][0]["response"]["status"], "200 OK");
    let carried = &answered["entry"][0]["resource"];
    assert_eq!(carried["type"], "searchset");
    assert_eq!(carried["entry"][0]["resource"]["id"], "d1");
}

#[tokio::test]
async fn a_deferred_interaction_that_fails_reports_it_in_the_entry() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);

    let reply = ask(
        &app,
        "GET",
        "/Patient?nosuchthing=1",
        Some("respond-async"),
        b"",
    )
    .await;
    let id = submitted_id(&reply);
    run(&app, &jobs).await;

    let polled = ask(&app, "GET", &format!("/_jobs/{id}"), None, b"").await;

    assert_eq!(polled.status, StatusCode::OK, "{}", polled.body);
    let answered = json(&polled.body);
    assert_eq!(answered["type"], "batch-response");
    assert_eq!(
        answered["entry"][0]["response"]["status"],
        "400 Bad Request"
    );
    assert_eq!(
        answered["entry"][0]["response"]["outcome"]["resourceType"],
        "OperationOutcome"
    );
}

#[tokio::test]
async fn a_deferred_interaction_that_is_cancelled_is_no_longer_found() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let reply = ask(&app, "GET", "/Patient", Some("respond-async"), b"").await;
    let id = submitted_id(&reply);

    let lifted = ask(&app, "DELETE", &format!("/_jobs/{id}"), None, b"").await;
    assert_eq!(lifted.status, StatusCode::ACCEPTED, "{}", lifted.body);

    let polled = ask(&app, "GET", &format!("/_jobs/{id}"), None, b"").await;
    assert_eq!(polled.status, StatusCode::NOT_FOUND, "{}", polled.body);
}

#[tokio::test]
async fn an_instance_asked_to_answer_later_keeps_answering_at_once() {
    let (jobs, _ticker) = queue();
    let app = service(Arc::clone(&jobs) as Arc<dyn JobStore>);
    let created = ask(&app, "POST", "/Patient", None, &patient("d2", "Stone")).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let reply = ask(&app, "GET", "/Patient/d2", Some("respond-async"), b"").await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(json(&reply.body)["id"], "d2");
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or(Value::Null)
}
