use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Service, Versioning};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    etag: String,
    body: String,
}

fn service(setting: &str) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let held = Versioning::parse(setting, FhirVersion::R4).expect("the setting reads");
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).with_versioning(held)
}

async fn ask(app: &Service, method: &str, uri: &str, etag: Option<&str>, body: &[u8]) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json");
    if let Some(etag) = etag {
        builder = builder.header("if-match", etag);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        etag,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn patient(id: &str, active: bool) -> Vec<u8> {
    serde_json::json!({"resourceType": "Patient", "id": id, "active": active})
        .to_string()
        .into_bytes()
}

fn json(reply: &Reply) -> Value {
    serde_json::from_str(&reply.body).expect("a json body")
}

#[tokio::test]
async fn a_type_held_under_versioned_update_refuses_a_write_that_names_no_version() {
    let app = service("versioned;Patient=versioned-update");
    let created = ask(&app, "PUT", "/Patient/v-1", None, &patient("v-1", true)).await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "a first write creates and names no version to replace: {}",
        created.body
    );
    let blind = ask(&app, "PUT", "/Patient/v-1", None, &patient("v-1", false)).await;
    assert_eq!(
        blind.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        blind.body
    );
    assert!(
        blind.body.contains("versioned-update"),
        "the outcome names the policy: {}",
        blind.body
    );
    let named = ask(
        &app,
        "PUT",
        "/Patient/v-1",
        Some(&created.etag),
        &patient("v-1", false),
    )
    .await;
    assert_eq!(named.status, StatusCode::OK, "{}", named.body);
}

#[tokio::test]
async fn a_patch_of_a_type_held_under_versioned_update_needs_a_version_too() {
    let app = service("Patient=versioned-update");
    let created = ask(&app, "PUT", "/Patient/v-2", None, &patient("v-2", true)).await;
    let patch = serde_json::json!([{"op": "replace", "path": "/active", "value": false}])
        .to_string()
        .into_bytes();
    let mut builder = Request::builder()
        .method("PATCH")
        .uri("/Patient/v-2")
        .header("host", "localhost")
        .header("content-type", "application/json-patch+json");
    builder = builder.header("accept", "application/fhir+json");
    let response = app
        .router()
        .oneshot(builder.body(Body::from(patch.clone())).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    let named = app
        .router()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/Patient/v-2")
                .header("host", "localhost")
                .header("content-type", "application/json-patch+json")
                .header("if-match", &created.etag)
                .body(Body::from(patch))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(named.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_type_left_alone_is_written_without_a_version() {
    let app = service("versioned;Patient=versioned-update");
    let created = ask(
        &app,
        "PUT",
        "/Observation/v-3",
        None,
        &serde_json::json!({"resourceType": "Observation", "id": "v-3", "status": "final",
                            "code": {"text": "probe"}})
        .to_string()
        .into_bytes(),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let again = ask(
        &app,
        "PUT",
        "/Observation/v-3",
        None,
        &serde_json::json!({"resourceType": "Observation", "id": "v-3", "status": "amended",
                            "code": {"text": "probe"}})
        .to_string()
        .into_bytes(),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
}

#[tokio::test]
async fn a_type_held_under_no_version_keeps_no_past() {
    let app = service("Patient=no-version");
    ask(&app, "PUT", "/Patient/v-4", None, &patient("v-4", true)).await;
    let second = ask(&app, "PUT", "/Patient/v-4", None, &patient("v-4", false)).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    let earlier = ask(&app, "GET", "/Patient/v-4/_history/1", None, b"").await;
    assert_eq!(
        earlier.status,
        StatusCode::NOT_FOUND,
        "the version behind the current one is gone: {}",
        earlier.body
    );
    let history = ask(&app, "GET", "/Patient/v-4/_history", None, b"").await;
    assert_eq!(history.status, StatusCode::OK, "{}", history.body);
    assert_eq!(json(&history)["total"], 1, "{}", history.body);
    let current = ask(&app, "GET", "/Patient/v-4", None, b"").await;
    assert_eq!(json(&current)["active"], false, "{}", current.body);
}

#[tokio::test]
async fn a_type_kept_versioned_still_keeps_its_past() {
    let app = service("Patient=no-version");
    let body = |status: &str| {
        serde_json::json!({"resourceType": "Observation", "id": "v-5", "status": status,
                           "code": {"text": "probe"}})
        .to_string()
        .into_bytes()
    };
    ask(&app, "PUT", "/Observation/v-5", None, &body("final")).await;
    ask(&app, "PUT", "/Observation/v-5", None, &body("amended")).await;
    let earlier = ask(&app, "GET", "/Observation/v-5/_history/1", None, b"").await;
    assert_eq!(earlier.status, StatusCode::OK, "{}", earlier.body);
}

#[tokio::test]
async fn the_statement_declares_the_policy_each_type_actually_has() {
    let app = service("versioned;Patient=versioned-update;AuditEvent=no-version");
    let reply = ask(&app, "GET", "/metadata", None, b"").await;
    let statement = json(&reply);
    let policy = |name: &str| -> String {
        statement["rest"][0]["resource"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["type"] == name)
            .unwrap()["versioning"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(policy("Patient"), "versioned-update");
    assert_eq!(policy("AuditEvent"), "no-version");
    assert_eq!(policy("Observation"), "versioned");
}

#[tokio::test]
async fn nothing_configured_leaves_every_type_versioned() {
    let app = service("");
    let created = ask(&app, "PUT", "/Patient/v-6", None, &patient("v-6", true)).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let again = ask(&app, "PUT", "/Patient/v-6", None, &patient("v-6", false)).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    let earlier = ask(&app, "GET", "/Patient/v-6/_history/1", None, b"").await;
    assert_eq!(earlier.status, StatusCode::OK, "{}", earlier.body);
}
