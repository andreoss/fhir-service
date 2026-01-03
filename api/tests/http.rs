use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

const LAST_MODIFIED: &str = "Sun, 06 Sep 2026 04:00:00 GMT";

struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Ok(())),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
}

async fn request(
    app: &Service,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder().method(method).uri(uri).header("host", "localhost");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or_default().to_owned()))
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
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn patient(id: &str, active: bool) -> Vec<u8> {
    format!(r#"{{"resourceType":"Patient","id":"{id}","active":{active}}}"#).into_bytes()
}

#[tokio::test]
async fn health_reports_ok_with_dependencies() {
    let app = service();
    let reply = request(&app, "GET", "/health", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "content-type"), "application/json");
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["status"], "ok");
    assert_eq!(value["dependencies"][0]["name"], "memory-store");
    assert_eq!(value["dependencies"][0]["status"], "ok");
}

#[tokio::test]
async fn health_reports_unhealthy_dependency_as_503() {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "broken",
        check: Arc::new(|| Err("down".to_owned())),
    }];
    let app = Service::new(Arc::new(store), FhirVersion::R4, dependencies);
    let reply = request(&app, "GET", "/health", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["status"], "degraded");
    assert_eq!(value["dependencies"][0]["status"], "error");
}

#[tokio::test]
async fn create_returns_location_etag_and_last_modified() {
    let app = service();
    let reply = request(&app, "POST", "/Patient", &[], &patient("pt-1", true)).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(header(&reply, "content-type"), "application/fhir+json");
    assert_eq!(header(&reply, "etag"), "W/\"1\"");
    assert_eq!(header(&reply, "last-modified"), LAST_MODIFIED);
    assert!(
        header(&reply, "location").ends_with("/Patient/pt-1/_history/1"),
        "location was {}",
        header(&reply, "location")
    );
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "Patient");
    assert_eq!(value["id"], "pt-1");
    assert_eq!(value["meta"]["versionId"], "1");
}

#[tokio::test]
async fn create_without_id_assigns_server_id() {
    let app = service();
    let body = br#"{"resourceType":"Patient","active":true}"#.to_vec();
    let reply = request(&app, "POST", "/Patient", &[], &body).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    let id = value["id"].as_str().expect("server must assign an id");
    assert!(!id.is_empty());
    let location = header(&reply, "location");
    assert!(location.ends_with(&format!("/Patient/{id}/_history/1")));
}

#[tokio::test]
async fn create_duplicate_id_is_rejected_with_409_outcome() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-2", true)).await;
    let reply = request(&app, "POST", "/Patient", &[], &patient("pt-2", false)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(header(&reply, "content-type"), "application/fhir+json");
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["code"], "duplicate");
}

#[tokio::test]
async fn create_with_mismatched_type_is_rejected() {
    let app = service();
    let reply = request(&app, "POST", "/Patient", &[], &patient("pt-3", true)).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = request(&app, "GET", "/Observation/pt-3", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_rejects_non_object_nothing() {
    let app = service();
    let reply = request(&app, "POST", "/Patient", &[], b"[1,2,3]").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn read_returns_current_version_with_headers() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-4", true)).await;
    let reply = request(&app, "GET", "/Patient/pt-4", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"1\"");
    assert_eq!(header(&reply, "last-modified"), LAST_MODIFIED);
    assert!(
        header(&reply, "content-location").ends_with("/Patient/pt-4/_history/1"),
        "content-location was {}",
        header(&reply, "content-location")
    );
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["id"], "pt-4");
    assert_eq!(value["active"], true);
}

#[tokio::test]
async fn read_unknown_id_returns_outcome_404() {
    let app = service();
    let reply = request(&app, "GET", "/Patient/nobody", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["code"], "not-found");
}

#[tokio::test]
async fn vread_returns_historical_version_and_rejects_unknown() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-5", true)).await;
    let reply = request(&app, "PUT", "/Patient/pt-5", &[("if-match", "W/\"1\"")], &patient("pt-5", false)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");

    let old = request(&app, "GET", "/Patient/pt-5/_history/1", &[], &[]).await;
    assert_eq!(old.status, StatusCode::OK);
    assert_eq!(header(&old, "etag"), "W/\"1\"");
    assert!(
        header(&old, "content-location").ends_with("/Patient/pt-5/_history/1"),
        "content-location was {}",
        header(&old, "content-location")
    );
    assert!(old.body.contains("\"active\":true"));

    let current = request(&app, "GET", "/Patient/pt-5/_history/2", &[], &[]).await;
    assert_eq!(current.status, StatusCode::OK);
    assert_eq!(header(&current, "etag"), "W/\"2\"");

    let missing = request(&app, "GET", "/Patient/pt-5/_history/99", &[], &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let value: serde_json::Value = serde_json::from_str(&missing.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "not-found");
}

#[tokio::test]
async fn update_with_current_if_match_creates_new_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-6", true)).await;
    let reply = request(&app, "PUT", "/Patient/pt-6", &[("if-match", "W/\"1\"")], &patient("pt-6", false)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
    assert_eq!(header(&reply, "last-modified"), LAST_MODIFIED);
    assert!(header(&reply, "location").ends_with("/Patient/pt-6/_history/2"));
}

#[tokio::test]
async fn update_with_stale_if_match_conflicts_with_409_outcome() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-7", true)).await;
    request(&app, "PUT", "/Patient/pt-7", &[("if-match", "W/\"1\"")], &patient("pt-7", false)).await;
    let reply = request(&app, "PUT", "/Patient/pt-7", &[("if-match", "W/\"1\"")], &patient("pt-7", true)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "conflict");
}

#[tokio::test]
async fn update_without_if_match_writes_new_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-8", true)).await;
    let reply = request(&app, "PUT", "/Patient/pt-8", &[], &patient("pt-8", false)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
}

#[tokio::test]
async fn noop_update_creates_no_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-9", true)).await;
    let reply = request(&app, "PUT", "/Patient/pt-9", &[("if-match", "W/\"1\"")], &patient("pt-9", true)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"1\"", "a no-op update must not advance the version");
    let read = request(&app, "GET", "/Patient/pt-9", &[], &[]).await;
    assert_eq!(header(&read, "etag"), "W/\"1\"");
}

#[tokio::test]
async fn update_with_body_id_mismatch_is_rejected() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-10", true)).await;
    let body = patient("other", true);
    let reply = request(&app, "PUT", "/Patient/pt-10", &[], &body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn update_unknown_id_returns_outcome_404() {
    let app = service();
    let reply = request(&app, "PUT", "/Patient/nobody", &[], &patient("nobody", true)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "not-found");
}

#[tokio::test]
async fn invalid_if_match_header_is_rejected() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-11", true)).await;
    let reply = request(&app, "PUT", "/Patient/pt-11", &[("if-match", "\"1\"")], &patient("pt-11", true)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn malformed_id_path_returns_outcome_400() {
    let app = service();
    let reply = request(&app, "GET", "/Patient/bad%20id", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn unknown_route_returns_outcome_404() {
    let app = service();
    let reply = request(&app, "GET", "/not/a/route", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "not-found");
}

#[tokio::test]
async fn unsupported_method_returns_outcome_405() {
    let app = service();
    let reply = request(&app, "DELETE", "/Patient/pt-x", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["code"], "not-allowed");
}