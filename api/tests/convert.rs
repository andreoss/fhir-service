use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn ask(app: &Service, content_type: &str, body: &[u8]) -> (StatusCode, String, String) {
    asking(app, content_type, "application/fhir+xml", body).await
}

async fn asking(
    app: &Service,
    content_type: &str,
    accept: &str,
    body: &[u8],
) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/$convert")
        .header("host", "localhost")
        .header("accept", accept)
        .header("content-type", content_type)
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let answered = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        answered,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn patient() -> Vec<u8> {
    json!({
        "resourceType": "Patient",
        "id": "cv-1",
        "active": true,
        "name": [{"family": "Stone", "given": ["Ada"]}]
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn json_is_answered_as_xml() {
    let app = service();
    let (status, answered, body) = ask(&app, "application/fhir+json", &patient()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(answered.contains("xml"), "{answered}");
    assert!(body.contains("<Patient"), "{body}");
    assert!(body.contains("Stone"), "{body}");
}

#[tokio::test]
async fn xml_is_answered_as_json() {
    let app = service();
    let (_, _, xml) = ask(&app, "application/fhir+json", &patient()).await;
    let (status, answered, body) = asking(
        &app,
        "application/fhir+xml",
        "application/fhir+json",
        xml.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(answered.contains("json"), "{answered}");
    let held: Value = serde_json::from_str(&body).expect("a json resource");
    assert_eq!(held["resourceType"], "Patient");
    assert_eq!(held["name"][0]["family"], "Stone");
    assert_eq!(held["active"], true);
}

#[tokio::test]
async fn a_round_trip_returns_what_it_started_with() {
    let app = service();
    let (_, _, xml) = ask(&app, "application/fhir+json", &patient()).await;
    let (_, _, back) = asking(
        &app,
        "application/fhir+xml",
        "application/fhir+json",
        xml.as_bytes(),
    )
    .await;
    let held: Value = serde_json::from_str(&back).unwrap();
    let sent: Value = serde_json::from_slice(&patient()).unwrap();
    assert_eq!(held, sent, "{back}");
}

#[tokio::test]
async fn a_body_that_does_not_parse_is_refused() {
    let app = service();
    let (status, _, body) = ask(&app, "application/fhir+json", b"not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = asking(
        &app,
        "application/fhir+xml",
        "application/fhir+json",
        b"<not-a-resource/>",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_content_type_naming_no_representation_is_refused() {
    let app = service();
    let (status, _, body) = ask(&app, "text/csv", &patient()).await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE, "{body}");
}

#[tokio::test]
async fn a_resource_the_release_does_not_serve_is_refused() {
    let app = service();
    let body = json!({"resourceType": "Nonesuch", "id": "x"})
        .to_string()
        .into_bytes();
    let (status, _, told) = ask(&app, "application/fhir+json", &body).await;
    assert!(status.is_client_error(), "{status}: {told}");
}
