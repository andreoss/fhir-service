use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore, MemoryStore};
use fhir_api::{Dependency, Polling, Service};
use fhir_core::{FhirInstant, FhirVersion};
use fhir_jobs::{ExportJob, Orchestrator, Worker};
use fhir_store::{BulkStore, JobId, JobStore, ResourceStore, StepTicker};
use fhir_store_contract::fixture::patient;
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

struct Harness {
    app: Service,
    jobs: Arc<MemoryJobStore>,
    store: Arc<MemoryStore>,
    sink: Arc<MemoryBulkStore>,
    ticker: StepTicker,
}

fn harness() -> Harness {
    let store = Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })));
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let sink = Arc::new(MemoryBulkStore::new());
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    let app = Service::new(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
        FhirVersion::R4,
        dependencies,
    )
    .with_jobs(Arc::clone(&jobs) as Arc<dyn JobStore>)
    .with_outputs(Arc::clone(&sink) as Arc<dyn BulkStore>)
    .with_polling(Polling::new(ticker.ticker()));
    Harness {
        app,
        jobs,
        store,
        sink,
        ticker,
    }
}

async fn ask(app: &Service, method: &str, uri: &str, from: Option<&str>, body: &[u8]) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if let Some(from) = from {
        builder = builder.header("x-forwarded-for", from);
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

fn submitted(reply: &Reply) -> JobId {
    let location = header(reply, "content-location");
    let tail = location
        .rsplit('/')
        .next()
        .expect("a location ends in an id");
    JobId::parse(tail).expect("the announced id is valid")
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or(Value::Null)
}

async fn submitted_export(held: &Harness) -> JobId {
    let accepted = ask(&held.app, "POST", "/$export", None, b"{}").await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{}", accepted.body);
    submitted(&accepted)
}

async fn work(held: &Harness) {
    let orchestrator = Arc::new(Orchestrator::new().with(Arc::new(ExportJob::new(
        Arc::clone(&held.store) as Arc<dyn ResourceStore>,
        Arc::clone(&held.sink) as Arc<dyn BulkStore>,
    ))));
    let worker = Worker::new(
        Arc::clone(&held.jobs) as Arc<dyn JobStore>,
        orchestrator,
        "one",
        5_000,
    );
    worker.poll().await.unwrap();
}

#[tokio::test]
async fn a_client_that_polls_again_inside_the_wait_is_asked_to_wait() {
    let held = harness();
    let id = submitted_export(&held).await;

    let first = ask(&held.app, "GET", &format!("/_jobs/{id}"), None, b"").await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    let second = ask(&held.app, "GET", &format!("/_jobs/{id}"), None, b"").await;

    assert_eq!(
        second.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        second.body
    );
    assert_eq!(header(&second, "retry-after"), "1");
    let outcome = json(&second.body);
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["code"], "throttled");
    assert!(
        outcome["issue"][0]["diagnostics"]
            .as_str()
            .unwrap_or_default()
            .contains('1'),
        "{}",
        second.body
    );
}

#[tokio::test]
async fn a_client_that_keeps_the_wait_is_answered_as_before() {
    let held = harness();
    let id = submitted_export(&held).await;

    let first = ask(&held.app, "GET", &format!("/_jobs/{id}"), None, b"").await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    held.ticker.advance(1_000);
    let later = ask(&held.app, "GET", &format!("/_jobs/{id}"), None, b"").await;

    assert_eq!(later.status, StatusCode::ACCEPTED, "{}", later.body);
    assert_eq!(header(&later, "retry-after"), "1");
    assert!(!header(&later, "x-progress").is_empty());
}

#[tokio::test]
async fn a_client_that_polls_from_another_address_is_counted_apart() {
    let held = harness();
    let id = submitted_export(&held).await;

    let first = ask(
        &held.app,
        "GET",
        &format!("/_jobs/{id}"),
        Some("10.0.0.1"),
        b"",
    )
    .await;
    let second = ask(
        &held.app,
        "GET",
        &format!("/_jobs/{id}"),
        Some("10.0.0.2"),
        b"",
    )
    .await;

    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    assert_eq!(second.status, StatusCode::ACCEPTED, "{}", second.body);
}

#[tokio::test]
async fn two_jobs_of_one_client_are_counted_apart() {
    let held = harness();
    let one = submitted_export(&held).await;
    let two = submitted_export(&held).await;

    let first = ask(&held.app, "GET", &format!("/_jobs/{one}"), None, b"").await;
    let second = ask(&held.app, "GET", &format!("/_jobs/{two}"), None, b"").await;

    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    assert_eq!(second.status, StatusCode::ACCEPTED, "{}", second.body);
}

#[tokio::test]
async fn a_job_that_has_ended_carries_no_wait() {
    let held = harness();
    held.store
        .create(patient("p1", "Stone", true))
        .await
        .unwrap();
    let id = submitted_export(&held).await;
    work(&held).await;

    let first = ask(&held.app, "GET", &format!("/_jobs/{id}"), None, b"").await;
    let second = ask(&held.app, "GET", &format!("/_jobs/{id}"), None, b"").await;

    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    assert_eq!(json(&second.body)["state"], "completed");
}

#[tokio::test]
async fn an_unknown_job_is_refused_and_not_counted() {
    let held = harness();

    let first = ask(&held.app, "GET", "/_jobs/nobody", None, b"").await;
    let second = ask(&held.app, "GET", "/_jobs/nobody", None, b"").await;

    assert_eq!(first.status, StatusCode::NOT_FOUND, "{}", first.body);
    assert_eq!(second.status, StatusCode::NOT_FOUND, "{}", second.body);
}
