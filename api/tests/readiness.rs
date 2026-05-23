use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn service() -> (Service, fhir_api::Busy) {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let app = Service::new(Arc::new(store), FhirVersion::R4, Vec::new());
    let busy = app.busy();
    (app, busy)
}

async fn ask(app: &Service, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn an_idle_instance_is_up_and_ready() {
    let (app, _) = service();
    let (status, body) = ask(&app, "/$liveness").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = ask(&app, "/$readiness").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_busy_instance_is_up_but_not_ready() {
    let (app, busy) = service();
    let held = busy.during("a reindex");
    let (status, _) = ask(&app, "/$liveness").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "liveness stays 200: it is not to be restarted"
    );
    let (status, body) = ask(&app, "/$readiness").await;
    assert_eq!(status.as_u16(), 423, "{body}");
    assert!(
        body.contains("a reindex"),
        "it says what to wait for: {body}"
    );
    drop(held);
    let (status, _) = ask(&app, "/$readiness").await;
    assert_eq!(status, StatusCode::OK, "and is ready again after");
}

#[tokio::test]
async fn health_still_reports_the_dependency() {
    let (app, busy) = service();
    let _held = busy.during("a migration");
    let (status, body) = ask(&app, "/health").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "health answers about the engine, not about being busy: {body}"
    );
}

#[tokio::test]
async fn a_ready_instance_names_the_indexes_an_operator_asked_for() {
    let (app, _) = service();
    let (status, body) = ask(&app, "/$readiness").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("operator"), "{body}");

    let (app, _) = service();
    let app = app.with_tuning(vec![
        "tune_token_code".to_owned(),
        "tune_reference_subject".to_owned(),
    ]);
    let (status, body) = ask(&app, "/$readiness").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("tune_token_code"), "{body}");
    assert!(body.contains("tune_reference_subject"), "{body}");
}
