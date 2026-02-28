use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::{MemoryJobStore, MemoryStore};
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use fhir_store::{JobId, JobStore, StepTicker};
use http_body_util::BodyExt;
use serde_json::Value;
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

async fn asked(app: &Service, uri: &str, handling: Option<&str>) -> Reply {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "localhost");
    if let Some(handling) = handling {
        builder = builder.header("prefer", format!("handling={handling}"));
    }
    let request = builder.body(Body::empty()).unwrap();
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

async fn submitted(jobs: &Arc<MemoryJobStore>, reply: &Reply) -> Value {
    let location = header(reply, "content-location");
    let tail = location
        .rsplit('/')
        .next()
        .expect("a status location ends in an id");
    let id = JobId::parse(tail).expect("the announced id is valid");
    let record = jobs.fetch(&id).await.unwrap();
    serde_json::from_str(record.payload.as_deref().unwrap_or("{}")).expect("a payload is json")
}

#[tokio::test]
async fn a_lenient_kick_off_carries_on_without_the_parameter_it_does_not_know() {
    let (jobs, _ticker) = queue();
    let app = service(jobs.clone() as Arc<dyn JobStore>);
    let reply = asked(&app, "/$export?_elements=id", Some("lenient")).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let payload = submitted(&jobs, &reply).await;
    assert!(payload.get("_elements").is_none());
    assert_eq!(
        payload.get("_unsupported"),
        Some(&serde_json::json!(["_elements"]))
    );
}

#[tokio::test]
async fn a_strict_kick_off_refuses_the_parameter_it_does_not_know() {
    let (jobs, _ticker) = queue();
    let app = service(jobs.clone() as Arc<dyn JobStore>);
    let reply = asked(&app, "/$export?_elements=id", Some("strict")).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(reply.body.contains("OperationOutcome"));
    let plain = asked(&app, "/$export?_elements=id", None).await;
    assert_eq!(plain.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_lenient_kick_off_drops_a_preset_it_does_not_carry() {
    let (jobs, _ticker) = queue();
    let app = service(jobs.clone() as Arc<dyn JobStore>);
    let reply = asked(
        &app,
        "/$export?includeAssociatedData=_custom",
        Some("lenient"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let payload = submitted(&jobs, &reply).await;
    assert!(payload.get("includeAssociatedData").is_none());
    assert_eq!(
        payload.get("_unsupported"),
        Some(&serde_json::json!(["includeAssociatedData"]))
    );
}

#[tokio::test]
async fn a_lenient_kick_off_keeps_the_parameters_it_carries() {
    let (jobs, _ticker) = queue();
    let app = service(jobs.clone() as Arc<dyn JobStore>);
    let reply = asked(&app, "/$export?_type=Patient&_elements=id", Some("lenient")).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let payload = submitted(&jobs, &reply).await;
    assert_eq!(payload.get("_type"), Some(&serde_json::json!(["Patient"])));
    assert!(payload.get("_elements").is_none());
}
