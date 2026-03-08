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

fn meta_of(body: &str) -> Value {
    let held: Value = serde_json::from_str(body).expect("a parameters resource");
    held["parameter"]
        .as_array()
        .and_then(|items| items.first())
        .map(|entry| entry["valueMeta"].clone())
        .unwrap_or(Value::Null)
}

fn asked(meta: Value) -> Vec<u8> {
    json!({
        "resourceType": "Parameters",
        "parameter": [{"name": "meta", "valueMeta": meta}]
    })
    .to_string()
    .into_bytes()
}

async fn seeded(app: &Service, id: &str) {
    let body = json!({
        "resourceType": "Patient",
        "id": id,
        "meta": {"tag": [{"system": "urn:s", "code": "kept"}]},
        "active": true
    })
    .to_string()
    .into_bytes();
    let (status, told) = ask(app, "PUT", &format!("/Patient/{id}"), &body).await;
    assert!(status.is_success(), "{told}");
}

#[tokio::test]
async fn the_meta_of_an_instance_is_read_without_the_resource() {
    let app = service();
    seeded(&app, "mt-1").await;
    let (status, body) = ask(&app, "GET", "/Patient/mt-1/$meta", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = meta_of(&body);
    assert_eq!(held["tag"][0]["code"], "kept");
    assert!(held.get("versionId").is_none(), "{held}");
    assert!(held.get("lastUpdated").is_none(), "{held}");
}

#[tokio::test]
async fn a_tag_is_added_and_the_resource_keeps_everything_else() {
    let app = service();
    seeded(&app, "mt-2").await;
    let (status, body) = ask(
        &app,
        "POST",
        "/Patient/mt-2/$meta-add",
        &asked(json!({"tag": [{"system": "urn:s", "code": "added"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = meta_of(&body);
    let codes: Vec<&str> = held["tag"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tag| tag["code"].as_str())
        .collect();
    assert_eq!(codes, vec!["kept", "added"], "{held}");

    let (_, read) = ask(&app, "GET", "/Patient/mt-2", b"").await;
    let resource: Value = serde_json::from_str(&read).unwrap();
    assert_eq!(resource["active"], true, "the resource is otherwise itself");
    assert_eq!(resource["meta"]["versionId"], "2", "a change is a version");
}

#[tokio::test]
async fn a_security_label_and_a_profile_claim_are_added_too() {
    let app = service();
    seeded(&app, "mt-3").await;
    let (status, body) = ask(
        &app,
        "POST",
        "/Patient/mt-3/$meta-add",
        &asked(json!({
            "security": [{"system": "urn:s", "code": "R"}],
            "profile": ["http://example.test/StructureDefinition/p"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = meta_of(&body);
    assert_eq!(held["security"][0]["code"], "R");
    assert_eq!(
        held["profile"][0],
        "http://example.test/StructureDefinition/p"
    );
}

#[tokio::test]
async fn adding_the_same_label_twice_adds_it_once() {
    let app = service();
    seeded(&app, "mt-4").await;
    let same = asked(json!({"tag": [{"system": "urn:s", "code": "kept"}]}));
    ask(&app, "POST", "/Patient/mt-4/$meta-add", &same).await;
    let (_, body) = ask(&app, "POST", "/Patient/mt-4/$meta-add", &same).await;
    assert_eq!(meta_of(&body)["tag"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn a_label_is_removed_and_an_empty_set_goes_with_it() {
    let app = service();
    seeded(&app, "mt-5").await;
    let (status, body) = ask(
        &app,
        "POST",
        "/Patient/mt-5/$meta-delete",
        &asked(json!({"tag": [{"system": "urn:s", "code": "kept"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = meta_of(&body);
    assert!(held.get("tag").is_none(), "{held}");
}

#[tokio::test]
async fn a_label_the_resource_does_not_carry_is_left_alone() {
    let app = service();
    seeded(&app, "mt-6").await;
    let (status, body) = ask(
        &app,
        "POST",
        "/Patient/mt-6/$meta-delete",
        &asked(json!({"tag": [{"system": "urn:s", "code": "absent"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(meta_of(&body)["tag"][0]["code"], "kept");
}

#[tokio::test]
async fn what_the_server_owns_is_refused() {
    let app = service();
    seeded(&app, "mt-7").await;
    for name in ["versionId", "lastUpdated"] {
        let (status, body) = ask(
            &app,
            "POST",
            "/Patient/mt-7/$meta-add",
            &asked(json!({ name: "9" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(body.contains(name), "{body}");
    }
}

#[tokio::test]
async fn the_labels_a_type_carries_are_the_union_of_its_resources() {
    let app = service();
    seeded(&app, "mt-8").await;
    ask(
        &app,
        "POST",
        "/Patient/mt-8/$meta-add",
        &asked(json!({"tag": [{"system": "urn:s", "code": "other"}]})),
    )
    .await;
    let (status, body) = ask(&app, "GET", "/Patient/$meta", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = meta_of(&body);
    let codes: Vec<&str> = held["tag"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tag| tag["code"].as_str())
        .collect();
    assert!(codes.contains(&"kept"), "{codes:?}");
    assert!(codes.contains(&"other"), "{codes:?}");
}

#[tokio::test]
async fn the_labels_the_instance_carries_are_read_at_the_system_level_too() {
    let app = service();
    seeded(&app, "mt-9").await;
    let (status, body) = ask(&app, "GET", "/$meta", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(meta_of(&body)["tag"][0]["code"], "kept");
}

#[tokio::test]
async fn a_resource_that_is_not_there_is_not_found() {
    let app = service();
    let (status, _) = ask(&app, "GET", "/Patient/nobody/$meta", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
