use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use fhir_store::StepTicker;
use fhir_telemetry::{Dimensions, Held, Operation, Outcome, Telemetry};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

const NAME: &str = "Rossignol";
const PATIENT: &str = "pt-confidential-77";
const TOKEN: &str = "secret-bearer-value";

fn service(sink: &Held) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-07T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    let ticker = StepTicker::starting_at(0).ticker();
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
        .reporting(Arc::new(Telemetry::new(sink.sink(), ticker)))
}

async fn call(app: &Service, method: &str, uri: &str, body: &[u8]) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/fhir+json")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn patient(id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "resourceType": "Patient",
        "id": id,
        "name": [{"family": NAME}],
    }))
    .unwrap()
}

#[tokio::test]
async fn a_request_is_measured_under_the_operation_it_ran() {
    let sink = Held::default();
    let app = service(&sink);
    let telemetry = app.telemetry();
    let created = call(&app, "POST", "/Patient", &patient(PATIENT)).await;
    assert_eq!(created.0, StatusCode::CREATED);
    let held = assigned(&created.1);
    call(&app, "PUT", &format!("/Patient/{held}"), &patient(&held)).await;
    call(&app, "GET", &format!("/Patient/{held}"), b"").await;
    call(&app, "GET", "/Patient?family=Rossignol", b"").await;
    call(&app, "GET", "/Patient/absent-id", b"").await;
    let text = telemetry.exposition();
    assert!(text.contains("fhir_operation_total{operation=\"create\",outcome=\"success\"} 1"));
    assert!(text.contains("fhir_operation_total{operation=\"update\",outcome=\"success\"} 1"));
    assert!(text.contains("fhir_operation_total{operation=\"read\",outcome=\"success\"} 1"));
    assert!(text.contains("fhir_operation_total{operation=\"search\",outcome=\"success\"} 1"));
    assert!(text.contains("fhir_operation_total{operation=\"read\",outcome=\"client_fault\"} 1"));
    assert!(text.contains("fhir_operation_failure_total{operation=\"read\"} 1"));
    assert!(text.contains("fhir_operation_duration_ms_count{operation=\"read\",outcome=\"success\"} 1"));
}

#[tokio::test]
async fn a_bundle_and_a_history_request_are_measured_apart() {
    let sink = Held::default();
    let app = service(&sink);
    let telemetry = app.telemetry();
    let bundle = serde_json::json!({
        "resourceType": "Bundle",
        "type": "batch",
        "entry": [{"request": {"method": "GET", "url": "Patient/absent"}}],
    });
    call(&app, "POST", "/", &serde_json::to_vec(&bundle).unwrap()).await;
    call(&app, "GET", "/_history", b"").await;
    assert_eq!(
        telemetry.count(Dimensions::of(Operation::Bundle, Outcome::Success)),
        1
    );
    assert_eq!(
        telemetry.count(Dimensions::of(Operation::History, Outcome::Success)),
        1
    );
}

#[tokio::test]
async fn nothing_a_caller_supplied_reaches_telemetry() {
    let sink = Held::default();
    let app = service(&sink);
    let telemetry = app.telemetry();
    call(&app, "PUT", &format!("/Patient/{PATIENT}"), &patient(PATIENT)).await;
    call(&app, "GET", &format!("/Patient/{PATIENT}"), b"").await;
    call(&app, "GET", &format!("/Patient?family={NAME}"), b"").await;
    call(&app, "GET", &format!("/Patient/{PATIENT}/_history"), b"").await;
    call(&app, "DELETE", &format!("/Patient/{PATIENT}"), b"").await;
    call(&app, "GET", "/Patient?identifier=urn:mrn|4711", b"").await;
    let mut written = sink.lines();
    written.push(telemetry.exposition());
    let held = written.join("\n");
    for secret in [NAME, PATIENT, TOKEN, "4711", "urn:mrn", "family", "Bearer"] {
        assert!(!held.contains(secret), "{secret} reached telemetry");
    }
    assert!(held.contains("operation=read"));
}

#[tokio::test]
async fn many_distinct_resources_add_no_series() {
    let sink = Held::default();
    let app = service(&sink);
    let telemetry = app.telemetry();
    for step in 0..300 {
        call(&app, "GET", &format!("/Patient/pt-{step}"), b"").await;
    }
    assert_eq!(telemetry.series(), 1);
    let text = telemetry.exposition();
    let counters = text
        .lines()
        .filter(|line| line.starts_with("fhir_operation_total"))
        .count();
    assert_eq!(counters, 1);
    assert!(text.len() < 4_000);
}

fn assigned(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|held| held["id"].as_str().map(|id| id.to_owned()))
        .expect("an assigned id")
}
