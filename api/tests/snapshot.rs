use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{OnWrite, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const URL: &str = "http://example.test/StructureDefinition/snapped";

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .with_profile_validation(OnWrite::default())
}

async fn ask(app: &Service, method: &str, uri: &str, body: &[u8]) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn differential() -> Value {
    json!({
        "resourceType": "StructureDefinition",
        "id": "snapped",
        "url": URL,
        "name": "Snapped",
        "status": "active",
        "kind": "resource",
        "abstract": false,
        "type": "Patient",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
        "derivation": "constraint",
        "differential": {"element": [
            {"path": "Patient.identifier", "min": 1, "max": "*"}
        ]}
    })
}

#[tokio::test]
async fn a_differential_is_answered_with_its_snapshot() {
    let app = service();
    let (status, body) = ask(
        &app,
        "POST",
        "/StructureDefinition/$snapshot",
        differential().to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(held["resourceType"], "StructureDefinition");
    let elements = held["snapshot"]["element"].as_array().expect("a snapshot");
    assert!(elements.len() > 10, "{}", elements.len());
    let identifier = elements
        .iter()
        .find(|element| element["path"] == "Patient.identifier")
        .expect("the constrained element");
    assert_eq!(
        identifier["min"], 1,
        "what the differential stated is there"
    );
    let name = elements
        .iter()
        .find(|element| element["path"] == "Patient.name")
        .expect("an element the differential did not state");
    assert_eq!(name["min"], 0, "and the base's rules are there too");
}

#[tokio::test]
async fn an_existing_snapshot_is_replaced_rather_than_kept() {
    let app = service();
    let mut held = differential();
    held["snapshot"] = json!({"element": [{"path": "Patient", "min": 9, "max": "9"}]});
    let (status, body) = ask(
        &app,
        "POST",
        "/StructureDefinition/$snapshot",
        held.to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answered: Value = serde_json::from_str(&body).unwrap();
    let elements = answered["snapshot"]["element"].as_array().unwrap();
    assert!(elements.len() > 1, "the stale snapshot is gone");
}

#[tokio::test]
async fn a_body_that_is_not_a_definition_is_refused() {
    let app = service();
    let (status, body) = ask(
        &app,
        "POST",
        "/StructureDefinition/$snapshot",
        json!({"resourceType": "Patient"}).to_string().as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_profile_of_a_profile_is_refused() {
    let app = service();
    let mut held = differential();
    held["baseDefinition"] = json!("http://example.test/StructureDefinition/another");
    let (status, body) = ask(
        &app,
        "POST",
        "/StructureDefinition/$snapshot",
        held.to_string().as_bytes(),
    )
    .await;
    assert!(status.is_client_error(), "{status}: {body}");
    assert!(body.contains("profile of a profile"), "{body}");
}

#[tokio::test]
async fn a_snapshotted_profile_is_applied_whole() {
    let app = service();

    let (_, snapped) = ask(
        &app,
        "POST",
        "/StructureDefinition/$snapshot",
        differential().to_string().as_bytes(),
    )
    .await;
    let (status, body) = ask(
        &app,
        "PUT",
        "/StructureDefinition/snapped",
        snapped.as_bytes(),
    )
    .await;
    assert!(status.is_success(), "{body}");

    let patient = json!({
        "resourceType": "Patient",
        "id": "sn-1",
        "meta": {"profile": [URL]}
    })
    .to_string()
    .into_bytes();
    let (status, told) = ask(&app, "POST", "/Patient/$validate", &patient).await;
    assert_eq!(status, StatusCode::OK, "{told}");
    assert!(
        told.contains("Patient.identifier"),
        "the snapshot's cardinality is applied: {told}"
    );
}
