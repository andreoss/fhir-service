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

fn definition(name: &str) -> Value {
    json!({
        "resourceType": "StructureDefinition",
        "id": name.to_lowercase(),
        "url": format!("http://example.test/StructureDefinition/{name}"),
        "name": name,
        "status": "active",
        "kind": "resource",
        "abstract": false,
        "type": name,
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/DomainResource",
        "derivation": "specialization",
        "differential": {"element": [{"path": format!("{name}.note"), "min": 0, "max": "1"}]}
    })
}

async fn registering(app: &Service, name: &str) {
    let (status, told) = ask(
        app,
        "PUT",
        &format!("/StructureDefinition/{}", name.to_lowercase()),
        definition(name).to_string().as_bytes(),
    )
    .await;
    assert!(status.is_success(), "{told}");
}

#[tokio::test]
async fn a_type_no_publication_carries_is_refused_until_it_is_registered() {
    let app = service();
    let (status, body) = ask(&app, "GET", "/Contraption", b"").await;
    assert!(status.is_client_error(), "{status}: {body}");
}

#[tokio::test]
async fn a_registered_type_is_stored_read_searched_and_versioned() {
    let app = service();
    registering(&app, "Apparatus").await;

    let body = json!({"resourceType": "Apparatus", "id": "ap-1", "note": "one"})
        .to_string()
        .into_bytes();
    let (status, told) = ask(&app, "PUT", "/Apparatus/ap-1", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{told}");

    let (status, read) = ask(&app, "GET", "/Apparatus/ap-1", b"").await;
    assert_eq!(status, StatusCode::OK, "{read}");
    let held: Value = serde_json::from_str(&read).unwrap();
    assert_eq!(held["note"], "one");
    assert_eq!(held["meta"]["versionId"], "1");

    let again = json!({"resourceType": "Apparatus", "id": "ap-1", "note": "two"})
        .to_string()
        .into_bytes();
    let (status, told) = ask(&app, "PUT", "/Apparatus/ap-1", &again).await;
    assert_eq!(status, StatusCode::OK, "{told}");

    let (status, history) = ask(&app, "GET", "/Apparatus/ap-1/_history", b"").await;
    assert_eq!(status, StatusCode::OK, "{history}");
    let held: Value = serde_json::from_str(&history).unwrap();
    assert_eq!(held["total"], 2, "{history}");

    let (status, found) = ask(&app, "GET", "/Apparatus?_id=ap-1", b"").await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let held: Value = serde_json::from_str(&found).unwrap();
    assert_eq!(held["total"], 1, "{found}");
}

#[tokio::test]
async fn a_registered_type_is_not_listed_in_the_statement() {
    let app = service();
    registering(&app, "Gizmo").await;
    let (status, body) = ask(&app, "GET", "/metadata", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let statement: Value = serde_json::from_str(&body).unwrap();
    let listed = statement["rest"][0]["resource"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["type"] == "Gizmo");
    assert!(
        !listed,
        "the statement's binding is to the published list: {body}"
    );
}

#[tokio::test]
async fn a_definition_that_constrains_rather_than_declares_registers_nothing() {
    let app = service();
    let profile = json!({
        "resourceType": "StructureDefinition",
        "id": "narrowed",
        "url": "http://example.test/StructureDefinition/Narrowed",
        "name": "Narrowed",
        "status": "active",
        "kind": "resource",
        "abstract": false,
        "type": "Patient",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
        "derivation": "constraint",
        "differential": {"element": [{"path": "Patient.identifier", "min": 1, "max": "*"}]}
    });
    let (status, told) = ask(
        &app,
        "PUT",
        "/StructureDefinition/narrowed",
        profile.to_string().as_bytes(),
    )
    .await;
    assert!(status.is_success(), "{told}");
    let (status, body) = ask(&app, "GET", "/Narrowed", b"").await;
    assert!(status.is_client_error(), "{status}: {body}");
}

#[tokio::test]
async fn a_registered_type_takes_part_in_a_bundle() {
    let app = service();
    registering(&app, "Widgetry").await;
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": [{
            "resource": {"resourceType": "Widgetry", "id": "wd-1", "note": "held"},
            "request": {"method": "PUT", "url": "Widgetry/wd-1"}
        }]
    })
    .to_string();
    let (status, body) = ask(&app, "POST", "/", bundle.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, read) = ask(&app, "GET", "/Widgetry/wd-1", b"").await;
    assert_eq!(status, StatusCode::OK, "{read}");
}
