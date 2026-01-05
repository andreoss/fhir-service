use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::{HistoryPage, HistoryQuery, HistoryScope, ResourceStore, SearchPage, SearchQuery};
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
    assert!(
        header(&reply, "content-location").ends_with("/Patient/pt-1/_history/1"),
        "content-location was {}",
        header(&reply, "content-location")
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
    assert!(header(&reply, "content-location").ends_with("/Patient/pt-6/_history/2"));
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
    assert!(header(&reply, "content-location").ends_with("/Patient/pt-9/_history/1"));
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
    let reply = request(&app, "POST", "/Patient/pt-x", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["code"], "not-allowed");
}

struct FailingStore;

#[async_trait]
impl ResourceStore for FailingStore {
    async fn create(&self, _: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn read(&self, _: &ResourceId) -> Result<ResourceEnvelope, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn vread(&self, _: &ResourceId, _: &VersionId) -> Result<ResourceEnvelope, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn update(&self, _: ResourceEnvelope, _: Option<&VersionId>) -> Result<ResourceEnvelope, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn search(&self, _: &SearchQuery) -> Result<SearchPage, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn delete(&self, _: &ResourceId) -> Result<ResourceEnvelope, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn hard_delete(&self, _: &ResourceId) -> Result<(), Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn purge_history(&self, _: &ResourceId) -> Result<usize, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    async fn history(&self, _: &HistoryScope, _: &HistoryQuery) -> Result<HistoryPage, Error> {
        Err(Error::Internal("boom".to_owned()))
    }
    fn health(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[tokio::test]
async fn internal_store_failure_is_a_500_outcome_without_leaks() {
    let app = Service::new(Arc::new(FailingStore), FhirVersion::R4, vec![]);
    let reply = request(&app, "GET", "/Patient/boom", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header(&reply, "content-type"), "application/fhir+json");
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    let object = value.as_object().expect("outcome must be an object");
    assert_eq!(object.len(), 2, "outcome must expose only resourceType and issue");
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["code"], "processing");
    assert_eq!(value["issue"][0]["diagnostics"], "boom");
}
#[tokio::test]
async fn binding_port_zero_reports_the_assigned_port() {
    let bound = service()
        .bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("bind must succeed");
    let addr = bound.local_addr().expect("local address must be known");
    assert_ne!(addr.port(), 0, "an ephemeral bind must report a real port");
}

#[tokio::test]
async fn binding_a_taken_port_fails_instead_of_serving() {
    let taken = service()
        .bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("first bind must succeed");
    let addr = taken.local_addr().expect("local address must be known");
    let outcome = service().bind(addr).await;
    let error = match outcome {
        Ok(_) => panic!("a taken port must not bind twice"),
        Err(error) => error,
    };
    assert!(matches!(error, Error::Internal(_)), "error was {error:?}");
}

#[tokio::test]
async fn conditional_create_without_a_match_creates_the_resource() {
    let app = service();
    let reply = request(
        &app,
        "POST",
        "/Patient",
        &[("if-none-exist", "active=true")],
        &patient("pt-c1", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(header(&reply, "etag"), "W/\"1\"");
}

#[tokio::test]
async fn conditional_create_with_one_match_returns_the_existing_resource() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-c2", true)).await;
    let reply = request(
        &app,
        "POST",
        "/Patient",
        &[("if-none-exist", "_id=pt-c2")],
        &patient("pt-c3", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["id"], "pt-c2");
    assert_eq!(value["meta"]["versionId"], "1");
    let missing = request(&app, "GET", "/Patient/pt-c3", &[], &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn conditional_create_with_many_matches_is_412() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-c4", true)).await;
    request(&app, "POST", "/Patient", &[], &patient("pt-c5", true)).await;
    let reply = request(
        &app,
        "POST",
        "/Patient",
        &[("if-none-exist", "active=true")],
        &patient("pt-c6", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "multiple-matches");
}

#[tokio::test]
async fn conditional_create_without_parameters_is_rejected() {
    let app = service();
    let reply = request(&app, "POST", "/Patient", &[("if-none-exist", "")], &patient("pt-c7", true)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn conditional_update_without_a_match_creates_the_resource() {
    let app = service();
    let reply = request(&app, "PUT", "/Patient?_id=pt-u1", &[], &patient("pt-u1", true)).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert!(header(&reply, "location").ends_with("/Patient/pt-u1/_history/1"));
    let read = request(&app, "GET", "/Patient/pt-u1", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK);
}

#[tokio::test]
async fn conditional_update_without_a_match_assigns_a_server_id() {
    let app = service();
    let body = br#"{"resourceType":"Patient","active":false}"#.to_vec();
    let reply = request(&app, "PUT", "/Patient?active=false", &[], &body).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert!(!value["id"].as_str().unwrap_or_default().is_empty());
}

#[tokio::test]
async fn conditional_update_with_one_match_writes_the_next_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-u2", true)).await;
    let body = br#"{"resourceType":"Patient","active":false}"#.to_vec();
    let reply = request(&app, "PUT", "/Patient?_id=pt-u2", &[], &body).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["id"], "pt-u2");
    assert_eq!(value["active"], false);
}

#[tokio::test]
async fn conditional_update_honours_a_stale_if_match() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-u3", true)).await;
    let reply = request(
        &app,
        "PUT",
        "/Patient?_id=pt-u3",
        &[("if-match", "W/\"7\"")],
        &patient("pt-u3", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "conflict");
}

#[tokio::test]
async fn conditional_update_with_a_mismatched_body_id_is_rejected() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-u4", true)).await;
    let reply = request(&app, "PUT", "/Patient?_id=pt-u4", &[], &patient("pt-other", false)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn conditional_update_with_many_matches_is_412() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-u5", true)).await;
    request(&app, "POST", "/Patient", &[], &patient("pt-u6", true)).await;
    let reply = request(&app, "PUT", "/Patient?active=true", &[], &patient("pt-u5", false)).await;
    assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "multiple-matches");
}

#[tokio::test]
async fn conditional_update_without_parameters_is_rejected() {
    let app = service();
    let reply = request(&app, "PUT", "/Patient", &[], &patient("pt-u7", true)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn result_control_parameters_do_not_count_as_a_condition() {
    let app = service();
    let reply = request(&app, "PUT", "/Patient?_format=json", &[], &patient("pt-u8", true)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn conditional_parameters_are_percent_decoded() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-u9","identifier":[{"system":"urn:x","value":"a b"}]}"#.to_vec();
    request(&app, "POST", "/Patient", &[], &body).await;
    let reply = request(&app, "PUT", "/Patient?identifier=a%20b", &[], &body).await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn delete_marks_the_resource_and_reports_the_new_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d1", true)).await;
    let reply = request(&app, "DELETE", "/Patient/pt-d1", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
    assert!(reply.body.is_empty());
}

#[tokio::test]
async fn reading_a_deleted_resource_is_410_with_outcome() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d2", true)).await;
    request(&app, "DELETE", "/Patient/pt-d2", &[], &[]).await;
    let reply = request(&app, "GET", "/Patient/pt-d2", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::GONE);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "deleted");
}

#[tokio::test]
async fn earlier_versions_stay_readable_after_a_delete() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d3", true)).await;
    request(&app, "DELETE", "/Patient/pt-d3", &[], &[]).await;
    let live = request(&app, "GET", "/Patient/pt-d3/_history/1", &[], &[]).await;
    assert_eq!(live.status, StatusCode::OK);
    let marker = request(&app, "GET", "/Patient/pt-d3/_history/2", &[], &[]).await;
    assert_eq!(marker.status, StatusCode::GONE);
}

#[tokio::test]
async fn deleting_twice_stays_no_content() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d4", true)).await;
    request(&app, "DELETE", "/Patient/pt-d4", &[], &[]).await;
    let reply = request(&app, "DELETE", "/Patient/pt-d4", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn deleting_an_unknown_resource_is_404() {
    let app = service();
    let reply = request(&app, "DELETE", "/Patient/pt-none", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "not-found");
}

#[tokio::test]
async fn a_deleted_resource_is_restored_by_an_update() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d5", true)).await;
    request(&app, "DELETE", "/Patient/pt-d5", &[], &[]).await;
    let reply = request(&app, "PUT", "/Patient/pt-d5", &[], &patient("pt-d5", false)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"3\"");
    let read = request(&app, "GET", "/Patient/pt-d5", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK);
}

#[tokio::test]
async fn hard_delete_removes_the_history_too() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d6", true)).await;
    let reply = request(&app, "DELETE", "/Patient/pt-d6?_hardDelete=true", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let read = request(&app, "GET", "/Patient/pt-d6", &[], &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
    let old = request(&app, "GET", "/Patient/pt-d6/_history/1", &[], &[]).await;
    assert_eq!(old.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn purge_history_keeps_the_current_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d7", true)).await;
    request(&app, "PUT", "/Patient/pt-d7", &[], &patient("pt-d7", false)).await;
    let reply = request(&app, "POST", "/Patient/pt-d7/$purge-history", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "Parameters");
    assert_eq!(value["parameter"][0]["name"], "versionsPurged");
    assert_eq!(value["parameter"][0]["valueInteger"], 1);
    let current = request(&app, "GET", "/Patient/pt-d7", &[], &[]).await;
    assert_eq!(current.status, StatusCode::OK);
    let purged = request(&app, "GET", "/Patient/pt-d7/_history/1", &[], &[]).await;
    assert_eq!(purged.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn purge_history_of_an_unknown_resource_is_404() {
    let app = service();
    let reply = request(&app, "POST", "/Patient/pt-none/$purge-history", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn conditional_delete_with_one_match_deletes_it() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d8", true)).await;
    let reply = request(&app, "DELETE", "/Patient?_id=pt-d8", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let read = request(&app, "GET", "/Patient/pt-d8", &[], &[]).await;
    assert_eq!(read.status, StatusCode::GONE);
}

#[tokio::test]
async fn conditional_delete_without_a_match_is_404() {
    let app = service();
    let reply = request(&app, "DELETE", "/Patient?_id=pt-none", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn conditional_delete_with_many_matches_is_412() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d9", true)).await;
    request(&app, "POST", "/Patient", &[], &patient("pt-da", true)).await;
    let reply = request(&app, "DELETE", "/Patient?active=true", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "multiple-matches");
}

#[tokio::test]
async fn conditional_delete_without_parameters_is_rejected() {
    let app = service();
    let reply = request(&app, "DELETE", "/Patient", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn conditional_hard_delete_removes_the_history() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-db", true)).await;
    let reply = request(&app, "DELETE", "/Patient?_id=pt-db&_hardDelete=true", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let old = request(&app, "GET", "/Patient/pt-db/_history/1", &[], &[]).await;
    assert_eq!(old.status, StatusCode::NOT_FOUND);
}

const JSON_PATCH: &str = "application/json-patch+json";

#[tokio::test]
async fn json_patch_writes_the_next_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p1", true)).await;
    let reply = request(
        &app,
        "PATCH",
        "/Patient/pt-p1",
        &[("content-type", JSON_PATCH)],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["active"], false);
    assert_eq!(value["id"], "pt-p1");
}

#[tokio::test]
async fn path_patch_writes_the_next_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p2", true)).await;
    let body = br#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[
        {"name":"type","valueCode":"replace"},
        {"name":"path","valueString":"Patient.active"},
        {"name":"value","valueBoolean":false}]}]}"#;
    let reply = request(&app, "PATCH", "/Patient/pt-p2", &[], body).await;
    assert_eq!(reply.status, StatusCode::OK, "body was {}", reply.body);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["active"], false);
}

#[tokio::test]
async fn a_rejected_patch_writes_nothing() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p3", true)).await;
    let reply = request(
        &app,
        "PATCH",
        "/Patient/pt-p3",
        &[("content-type", JSON_PATCH)],
        br#"[{"op":"replace","path":"/active","value":false},{"op":"remove","path":"/gender"}]"#,
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let outcome: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(outcome["issue"][0]["code"], "invalid");
    let read = request(&app, "GET", "/Patient/pt-p3", &[], &[]).await;
    let value: serde_json::Value = serde_json::from_str(&read.body).unwrap();
    assert_eq!(value["meta"]["versionId"], "1");
    assert_eq!(value["active"], true);
}

#[tokio::test]
async fn a_patch_may_not_change_the_id_or_type() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p4", true)).await;
    for patch in [
        br#"[{"op":"replace","path":"/id","value":"other"}]"#.to_vec(),
        br#"[{"op":"replace","path":"/resourceType","value":"Observation"}]"#.to_vec(),
    ] {
        let reply = request(&app, "PATCH", "/Patient/pt-p4", &[("content-type", JSON_PATCH)], &patch).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "body was {}", reply.body);
    }
}

#[tokio::test]
async fn patch_honours_a_stale_if_match() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p5", true)).await;
    let reply = request(
        &app,
        "PATCH",
        "/Patient/pt-p5",
        &[("content-type", JSON_PATCH), ("if-match", "W/\"9\"")],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn patching_an_unknown_or_deleted_resource_fails() {
    let app = service();
    let unknown = request(
        &app,
        "PATCH",
        "/Patient/pt-none",
        &[("content-type", JSON_PATCH)],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    request(&app, "POST", "/Patient", &[], &patient("pt-p6", true)).await;
    request(&app, "DELETE", "/Patient/pt-p6", &[], &[]).await;
    let deleted = request(
        &app,
        "PATCH",
        "/Patient/pt-p6",
        &[("content-type", JSON_PATCH)],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::GONE);
}

#[tokio::test]
async fn a_malformed_patch_document_is_rejected() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p7", true)).await;
    let reply = request(
        &app,
        "PATCH",
        "/Patient/pt-p7",
        &[("content-type", JSON_PATCH)],
        br#"{"resourceType":"Patient"}"#,
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn conditional_patch_selects_the_single_match() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p8", true)).await;
    let reply = request(
        &app,
        "PATCH",
        "/Patient?_id=pt-p8",
        &[("content-type", JSON_PATCH)],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
}

#[tokio::test]
async fn conditional_patch_reports_no_match_and_many_matches() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-p9", true)).await;
    request(&app, "POST", "/Patient", &[], &patient("pt-pa", true)).await;
    let patch = br#"[{"op":"replace","path":"/active","value":false}]"#;
    let none = request(&app, "PATCH", "/Patient?_id=pt-none", &[("content-type", JSON_PATCH)], patch).await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    let many = request(&app, "PATCH", "/Patient?active=true", &[("content-type", JSON_PATCH)], patch).await;
    assert_eq!(many.status, StatusCode::PRECONDITION_FAILED);
    let empty = request(&app, "PATCH", "/Patient", &[("content-type", JSON_PATCH)], patch).await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
}

fn ticking_service() -> Service {
    let tick = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let store = MemoryStore::with_clock(Arc::new(move || {
        let second = tick.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FhirInstant::parse(&format!("2026-09-06T04:00:{second:02}Z")).unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Ok(())),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
}

async fn seeded_history() -> Service {
    let app = ticking_service();
    request(&app, "POST", "/Patient", &[], &patient("pt-h1", true)).await;
    request(&app, "PUT", "/Patient/pt-h1", &[], &patient("pt-h1", false)).await;
    request(&app, "DELETE", "/Patient/pt-h1", &[], &[]).await;
    request(&app, "POST", "/Observation", &[], br#"{"resourceType":"Observation","id":"ob-h1","status":"final"}"#).await;
    app
}

fn bundle(reply: &Reply) -> serde_json::Value {
    serde_json::from_str(&reply.body).expect("history bundle must be json")
}

fn entry_versions(value: &serde_json::Value) -> Vec<String> {
    value["entry"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["response"]["etag"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn link(value: &serde_json::Value, relation: &str) -> String {
    value["link"]
        .as_array()
        .and_then(|links| {
            links
                .iter()
                .find(|item| item["relation"] == relation)
                .and_then(|item| item["url"].as_str())
        })
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn instance_history_is_a_bundle_newest_first() {
    let app = seeded_history().await;
    let reply = request(&app, "GET", "/Patient/pt-h1/_history", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "content-type"), "application/fhir+json");
    let value = bundle(&reply);
    assert_eq!(value["resourceType"], "Bundle");
    assert_eq!(value["type"], "history");
    assert_eq!(value["total"], 3);
    assert_eq!(entry_versions(&value), ["W/\"3\"", "W/\"2\"", "W/\"1\""]);
    assert_eq!(value["entry"][0]["request"]["method"], "DELETE");
    assert_eq!(value["entry"][0]["request"]["url"], "Patient/pt-h1");
    assert_eq!(value["entry"][0]["response"]["status"], "204");
    assert!(value["entry"][0]["resource"].is_null(), "a delete marker carries no resource");
    assert_eq!(value["entry"][1]["request"]["method"], "PUT");
    assert_eq!(value["entry"][1]["request"]["url"], "Patient/pt-h1");
    assert_eq!(value["entry"][1]["response"]["status"], "200");
    assert_eq!(value["entry"][1]["resource"]["active"], false);
    assert_eq!(value["entry"][2]["request"]["method"], "POST");
    assert_eq!(value["entry"][2]["request"]["url"], "Patient");
    assert_eq!(value["entry"][2]["response"]["status"], "201");
    assert_eq!(value["entry"][2]["fullUrl"], "http://localhost/Patient/pt-h1");
    assert_eq!(value["entry"][2]["response"]["lastModified"], "2026-09-06T04:00:00Z");
}

#[tokio::test]
async fn type_and_system_history_span_the_right_resources() {
    let app = seeded_history().await;
    let typed = bundle(&request(&app, "GET", "/Patient/_history", &[], &[]).await);
    assert_eq!(typed["total"], 3);
    let system = bundle(&request(&app, "GET", "/_history", &[], &[]).await);
    assert_eq!(system["total"], 4);
    assert_eq!(system["entry"][0]["request"]["url"], "Observation");
    let other = bundle(&request(&app, "GET", "/Encounter/_history", &[], &[]).await);
    assert_eq!(other["total"], 0);
    assert!(other["entry"].is_null());
}

#[tokio::test]
async fn history_pages_through_a_continuation_token() {
    let app = seeded_history().await;
    let first = bundle(&request(&app, "GET", "/_history?_count=2", &[], &[]).await);
    assert_eq!(first["total"], 4);
    assert_eq!(first["entry"].as_array().unwrap().len(), 2);
    assert_eq!(link(&first, "self"), "http://localhost/_history?_count=2");
    let next = link(&first, "next");
    assert!(next.contains("ct="), "next link was {next}");
    let path = next.trim_start_matches("http://localhost").to_owned();
    let second = bundle(&request(&app, "GET", &path, &[], &[]).await);
    assert_eq!(entry_versions(&second), ["W/\"2\"", "W/\"1\""]);
    assert_eq!(link(&second, "next"), "", "the last page has no next link");
}

#[tokio::test]
async fn history_filters_by_write_time() {
    let app = seeded_history().await;
    let since = bundle(&request(&app, "GET", "/_history?_since=2026-09-06T04:00:02Z", &[], &[]).await);
    assert_eq!(since["total"], 2);
    let before = bundle(&request(&app, "GET", "/_history?_before=2026-09-06T04:00:01Z", &[], &[]).await);
    assert_eq!(before["total"], 1);
    let at = bundle(&request(&app, "GET", "/_history?_at=2026-09-06T04:00:01Z", &[], &[]).await);
    assert_eq!(at["total"], 1);
    let day = bundle(&request(&app, "GET", "/_history?_at=2026-09-06", &[], &[]).await);
    assert_eq!(day["total"], 4);
    let elsewhere = bundle(&request(&app, "GET", "/_history?_at=2025", &[], &[]).await);
    assert_eq!(elsewhere["total"], 0);
}

#[tokio::test]
async fn history_sorts_oldest_first_on_request() {
    let app = seeded_history().await;
    let value = bundle(&request(&app, "GET", "/Patient/pt-h1/_history?_sort=_lastUpdated", &[], &[]).await);
    assert_eq!(entry_versions(&value), ["W/\"1\"", "W/\"2\"", "W/\"3\""]);
    let reverse = bundle(&request(&app, "GET", "/Patient/pt-h1/_history?_sort=-_lastUpdated", &[], &[]).await);
    assert_eq!(entry_versions(&reverse), ["W/\"3\"", "W/\"2\"", "W/\"1\""]);
}

#[tokio::test]
async fn summary_count_reports_the_total_without_entries() {
    let app = seeded_history().await;
    let value = bundle(&request(&app, "GET", "/_history?_summary=count", &[], &[]).await);
    assert_eq!(value["total"], 4);
    assert!(value["entry"].is_null());
    let zero = bundle(&request(&app, "GET", "/_history?_count=0", &[], &[]).await);
    assert_eq!(zero["total"], 4);
    assert!(zero["entry"].is_null());
}

#[tokio::test]
async fn summary_true_carries_metadata_only() {
    let app = seeded_history().await;
    let value = bundle(&request(&app, "GET", "/Patient/pt-h1/_history?_summary=true", &[], &[]).await);
    let resource = &value["entry"][1]["resource"];
    assert_eq!(resource["resourceType"], "Patient");
    assert_eq!(resource["meta"]["versionId"], "2");
    assert!(resource["active"].is_null(), "the summary carries no body");
}

#[tokio::test]
async fn malformed_history_parameters_are_rejected() {
    let app = seeded_history().await;
    for query in [
        "_count=many",
        "_sort=name",
        "_summary=partial",
        "_since=whenever",
        "_at=2026-13",
        "ct=zz",
    ] {
        let reply = request(&app, "GET", &format!("/_history?{query}"), &[], &[]).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{query} must be rejected");
    }
}

#[tokio::test]
async fn history_of_an_unknown_instance_or_type_fails() {
    let app = seeded_history().await;
    let unknown = request(&app, "GET", "/Patient/pt-none/_history", &[], &[]).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let bad_type = request(&app, "GET", "/Nope/_history", &[], &[]).await;
    assert_eq!(bad_type.status, StatusCode::BAD_REQUEST);
}

fn tagged(id: &str, tag: &str) -> Vec<u8> {
    format!(
        r#"{{"resourceType":"Patient","id":"{id}","meta":{{"tag":[{{"system":"urn:t","code":"{tag}"}}],"profile":["http://x/vip"],"security":[{{"system":"urn:s","code":"R"}}]}},"active":true}}"#
    )
    .into_bytes()
}

fn entries(value: &serde_json::Value) -> Vec<String> {
    value["entry"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["resource"]["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn seeded() -> Service {
    let app = service();
    request(&app, "POST", "/Patient", &[], &tagged("pt-s1", "gold")).await;
    request(&app, "POST", "/Patient", &[], &tagged("pt-s2", "silver")).await;
    request(&app, "POST", "/Patient", &[], &patient("pt-s3", false)).await;
    let list = br#"{"resourceType":"List","id":"ls-1","status":"current","mode":"working","entry":[{"item":{"reference":"Patient/pt-s2"}}]}"#;
    request(&app, "POST", "/List", &[], list).await;
    app
}

#[tokio::test]
async fn a_type_search_returns_a_searchset_bundle() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "content-type"), "application/fhir+json");
    let value = bundle(&reply);
    assert_eq!(value["resourceType"], "Bundle");
    assert_eq!(value["type"], "searchset");
    assert_eq!(value["total"], 3);
    assert_eq!(value["entry"][0]["search"]["mode"], "match");
    assert_eq!(link(&value, "self"), "http://localhost/Patient");
}

#[tokio::test]
async fn a_search_selects_by_id_and_last_updated() {
    let app = seeded().await;
    let by_id = bundle(&request(&app, "GET", "/Patient?_id=pt-s2", &[], &[]).await);
    assert_eq!(entries(&by_id), vec!["pt-s2".to_owned()]);
    let recent = bundle(&request(&app, "GET", "/Patient?_lastUpdated=ge2026-09-06", &[], &[]).await);
    assert_eq!(recent["total"], 3);
    let old = bundle(&request(&app, "GET", "/Patient?_lastUpdated=lt2020", &[], &[]).await);
    assert_eq!(old["total"], 0);
    assert!(old["entry"].is_null());
}

#[tokio::test]
async fn a_search_selects_by_profile_tag_and_security() {
    let app = seeded().await;
    let tag = bundle(&request(&app, "GET", "/Patient?_tag=urn:t|gold", &[], &[]).await);
    assert_eq!(entries(&tag), vec!["pt-s1".to_owned()]);
    let profile = bundle(&request(&app, "GET", "/Patient?_profile=http://x/vip", &[], &[]).await);
    assert_eq!(profile["total"], 2);
    let security = bundle(&request(&app, "GET", "/Patient?_security=urn:s|R", &[], &[]).await);
    assert_eq!(security["total"], 2);
}

#[tokio::test]
async fn a_search_across_every_type_is_restricted_by_type() {
    let app = seeded().await;
    let all = bundle(&request(&app, "GET", "/", &[], &[]).await);
    assert_eq!(all["total"], 4);
    let patients = bundle(&request(&app, "GET", "/?_type=Patient", &[], &[]).await);
    assert_eq!(patients["total"], 3);
    let both = bundle(&request(&app, "GET", "/?_type=Patient,List", &[], &[]).await);
    assert_eq!(both["total"], 4);
}

#[tokio::test]
async fn a_search_selects_the_members_of_a_list() {
    let app = seeded().await;
    let members = bundle(&request(&app, "GET", "/Patient?_list=ls-1", &[], &[]).await);
    assert_eq!(entries(&members), vec!["pt-s2".to_owned()]);
}

#[tokio::test]
async fn a_deleted_resource_is_invisible_to_search() {
    let app = seeded().await;
    request(&app, "DELETE", "/Patient/pt-s3", &[], &[]).await;
    let value = bundle(&request(&app, "GET", "/Patient", &[], &[]).await);
    assert_eq!(value["total"], 2);
}

#[tokio::test]
async fn an_unsupported_search_parameter_is_rejected() {
    let app = seeded().await;
    for uri in ["/Patient?nonesuch=1", "/Patient?_include=Patient:link", "/Patient?_text=x", "/Patient?_type=Patient"] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{uri}");
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported", "{uri}");
    }
}

#[tokio::test]
async fn a_malformed_search_value_is_rejected() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient?_lastUpdated=whenever", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

async fn many() -> Service {
    let app = service();
    for index in 1..=5 {
        request(&app, "POST", "/Patient", &[], &patient(&format!("pt-r{index}"), true)).await;
    }
    app
}

#[tokio::test]
async fn count_pages_the_result_and_offers_a_next_link() {
    let app = many().await;
    let first = bundle(&request(&app, "GET", "/Patient?_count=2", &[], &[]).await);
    assert_eq!(first["total"], 5);
    assert_eq!(entries(&first), vec!["pt-r1".to_owned(), "pt-r2".to_owned()]);
    let next = link(&first, "next");
    assert!(next.contains("ct="), "next was {next}");
    let token = next.rsplit("ct=").next().unwrap().to_owned();
    let second = bundle(&request(&app, "GET", &format!("/Patient?_count=2&ct={token}"), &[], &[]).await);
    assert_eq!(entries(&second), vec!["pt-r3".to_owned(), "pt-r4".to_owned()]);
    let last = bundle(&request(&app, "GET", "/Patient?_count=10", &[], &[]).await);
    assert_eq!(last["link"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_tampered_continuation_token_is_rejected() {
    let app = many().await;
    let reply = request(&app, "GET", "/Patient?ct=zzzz", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn sort_orders_by_a_validated_parameter() {
    let app = many().await;
    let down = bundle(&request(&app, "GET", "/Patient?_sort=-_id", &[], &[]).await);
    assert_eq!(entries(&down).first(), Some(&"pt-r5".to_owned()));
    let up = bundle(&request(&app, "GET", "/Patient?_sort=_id", &[], &[]).await);
    assert_eq!(entries(&up).first(), Some(&"pt-r1".to_owned()));
    let by_time = bundle(&request(&app, "GET", "/Patient?_sort=_lastUpdated,_id", &[], &[]).await);
    assert_eq!(by_time["total"], 5);
}

#[tokio::test]
async fn sort_on_an_unsortable_parameter_is_rejected() {
    let app = many().await;
    for uri in ["/Patient?_sort=active", "/Patient?_sort=nonesuch"] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{uri}");
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported", "{uri}");
    }
}

#[tokio::test]
async fn total_is_accurate_estimated_or_absent() {
    let app = many().await;
    assert_eq!(bundle(&request(&app, "GET", "/Patient?_total=accurate", &[], &[]).await)["total"], 5);
    assert_eq!(bundle(&request(&app, "GET", "/Patient?_total=estimate", &[], &[]).await)["total"], 5);
    let none = bundle(&request(&app, "GET", "/Patient?_total=none", &[], &[]).await);
    assert!(none["total"].is_null());
    let reply = request(&app, "GET", "/Patient?_total=guess", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn elements_and_summary_narrow_every_entry() {
    let app = seeded().await;
    let elements = bundle(&request(&app, "GET", "/Patient?_elements=active", &[], &[]).await);
    let first = &elements["entry"][0]["resource"];
    assert!(first["active"].is_boolean());
    assert!(first["id"].is_string());
    let tags = first["meta"]["tag"].as_array().unwrap();
    assert!(tags.iter().any(|tag| tag["code"] == "SUBSETTED"));
    let counted = bundle(&request(&app, "GET", "/Patient?_summary=count", &[], &[]).await);
    assert_eq!(counted["total"], 3);
    assert!(counted["entry"].is_null());
    let brief = bundle(&request(&app, "GET", "/Patient?_summary=true", &[], &[]).await);
    assert!(brief["entry"][0]["resource"]["id"].is_string());
    let reply = request(&app, "GET", "/Patient?_summary=partial", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn format_selects_a_supported_rendering() {
    let app = seeded().await;
    for uri in ["/Patient?_format=json", "/Patient?_format=application/fhir%2Bjson"] {
        assert_eq!(request(&app, "GET", uri, &[], &[]).await.status, StatusCode::OK, "{uri}");
    }
    let reply = request(&app, "GET", "/Patient?_format=xml", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "not-supported");
}

#[tokio::test]
async fn a_malformed_count_is_rejected() {
    let app = many().await;
    let reply = request(&app, "GET", "/Patient?_count=many", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

async fn clinical() -> Service {
    let app = service();
    let ana = br#"{"resourceType":"Patient","id":"pt-v1","name":[{"family":"de la Cruz","given":["Ana"]}],"birthDate":"1980-04-01","managingOrganization":{"reference":"Organization/org-1"}}"#;
    let bo = br#"{"resourceType":"Patient","id":"pt-v2","name":[{"family":"Okonkwo","given":["Bo"]}],"birthDate":"1995-11-20"}"#;
    request(&app, "POST", "/Patient", &[], ana).await;
    request(&app, "POST", "/Patient", &[], bo).await;
    let rate = br#"{"resourceType":"Observation","id":"ob-v1","status":"final","code":{"coding":[{"system":"http://loinc.org","code":"8867-4"}]},"subject":{"reference":"Patient/pt-v1"},"effectiveDateTime":"2026-09-06T04:00:00Z","valueQuantity":{"value":72.5,"system":"http://unitsofmeasure.org","code":"/min"}}"#;
    let pressure = br#"{"resourceType":"Observation","id":"ob-v2","status":"final","code":{"coding":[{"system":"http://loinc.org","code":"85354-9"}]},"subject":{"reference":"Patient/pt-v2"},"effectiveDateTime":"2026-09-06T04:00:00Z","component":[{"code":{"coding":[{"system":"http://loinc.org","code":"8480-6"}]},"valueQuantity":{"value":120,"system":"http://unitsofmeasure.org","code":"mm[Hg]"}}]}"#;
    request(&app, "POST", "/Observation", &[], rate).await;
    request(&app, "POST", "/Observation", &[], pressure).await;
    let risk = br#"{"resourceType":"RiskAssessment","id":"ra-v1","subject":{"reference":"Patient/pt-v1"},"prediction":[{"probabilityDecimal":0.42}]}"#;
    request(&app, "POST", "/RiskAssessment", &[], risk).await;
    app
}

async fn found(app: &Service, uri: &str) -> Vec<String> {
    let reply = request(app, "GET", uri, &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri} gave {}", reply.body);
    entries(&bundle(&reply))
}

#[tokio::test]
async fn a_search_matches_string_and_date_values() {
    let app = clinical().await;
    assert_eq!(found(&app, "/Patient?family=de%20la").await, vec!["pt-v1".to_owned()]);
    assert_eq!(found(&app, "/Patient?given=BO").await, vec!["pt-v2".to_owned()]);
    assert_eq!(found(&app, "/Patient?birthdate=lt1990").await, vec!["pt-v1".to_owned()]);
    assert_eq!(found(&app, "/Patient?birthdate=1995-11-20").await, vec!["pt-v2".to_owned()]);
}

#[tokio::test]
async fn a_search_matches_token_reference_and_uri_values() {
    let app = clinical().await;
    assert_eq!(found(&app, "/Observation?code=http://loinc.org|8867-4").await, vec!["ob-v1".to_owned()]);
    assert_eq!(found(&app, "/Observation?subject=Patient/pt-v2").await, vec!["ob-v2".to_owned()]);
    assert_eq!(found(&app, "/Observation?patient=pt-v1").await, vec!["ob-v1".to_owned()]);
    assert_eq!(found(&app, "/Observation?status=final").await.len(), 2);
    assert_eq!(found(&app, "/Patient?organization=Organization/org-1").await, vec!["pt-v1".to_owned()]);
}

#[tokio::test]
async fn a_search_matches_quantity_number_and_composite_values() {
    let app = clinical().await;
    assert_eq!(
        found(&app, "/Observation?value-quantity=72.5|http://unitsofmeasure.org|/min").await,
        vec!["ob-v1".to_owned()]
    );
    assert_eq!(found(&app, "/Observation?value-quantity=gt70").await, vec!["ob-v1".to_owned()]);
    assert!(found(&app, "/Observation?value-quantity=lt70").await.is_empty());
    assert_eq!(found(&app, "/RiskAssessment?probability=0.42").await, vec!["ra-v1".to_owned()]);
    assert_eq!(
        found(&app, "/Observation?component-code-value-quantity=8480-6$120").await,
        vec!["ob-v2".to_owned()]
    );
    assert!(found(&app, "/Observation?component-code-value-quantity=8867-4$120").await.is_empty());
}

#[tokio::test]
async fn a_malformed_composite_value_is_rejected() {
    let app = clinical().await;
    let reply = request(&app, "GET", "/Observation?component-code-value-quantity=8480-6", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn a_parameter_of_another_type_is_not_accepted() {
    let app = clinical().await;
    let reply = request(&app, "GET", "/Patient?value-quantity=72.5", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "not-supported");
}

#[tokio::test]
async fn sorting_orders_by_a_typed_parameter() {
    let app = clinical().await;
    assert_eq!(
        found(&app, "/Patient?_sort=birthdate").await,
        vec!["pt-v1".to_owned(), "pt-v2".to_owned()]
    );
    assert_eq!(
        found(&app, "/Patient?_sort=-family").await,
        vec!["pt-v2".to_owned(), "pt-v1".to_owned()]
    );
}

async fn qualified() -> Service {
    let app = service();
    let ann = br#"{"resourceType":"Patient","id":"pt-m1","name":[{"family":"Sorensen","given":["Ann"]}],"gender":"female","identifier":[{"type":{"coding":[{"system":"urn:t","code":"MR"}]},"system":"urn:mrn","value":"12345"}]}"#;
    let bo = br#"{"resourceType":"Patient","id":"pt-m2","name":[{"family":"Okonkwo"}],"gender":"male"}"#;
    request(&app, "POST", "/Patient", &[], ann).await;
    request(&app, "POST", "/Patient", &[], bo).await;
    let warm = br#"{"resourceType":"Observation","id":"ob-m1","status":"final","code":{"text":"Body Temperature","coding":[{"system":"urn:s","code":"vital.temperature"}]},"subject":{"reference":"Patient/pt-m1"}}"#;
    let other = br#"{"resourceType":"Observation","id":"ob-m2","status":"registered","code":{"coding":[{"system":"urn:s","code":"survey"}]},"subject":{"identifier":{"system":"urn:mrn","value":"12345"}}}"#;
    request(&app, "POST", "/Observation", &[], warm).await;
    request(&app, "POST", "/Observation", &[], other).await;
    let set = br#"{"resourceType":"ValueSet","id":"vs-m1","url":"http://x/vitals","status":"active","compose":{"include":[{"system":"urn:s","concept":[{"code":"vital.temperature"}]}]}}"#;
    request(&app, "POST", "/ValueSet", &[], set).await;
    app
}

#[tokio::test]
async fn string_modifiers_narrow_a_search() {
    let app = qualified().await;
    assert_eq!(found(&app, "/Patient?family:exact=Okonkwo").await, vec!["pt-m2".to_owned()]);
    assert!(found(&app, "/Patient?family:exact=okonkwo").await.is_empty());
    assert_eq!(found(&app, "/Patient?family:contains=oren").await, vec!["pt-m1".to_owned()]);
}

#[tokio::test]
async fn the_missing_modifier_selects_by_presence() {
    let app = qualified().await;
    assert_eq!(found(&app, "/Patient?identifier:missing=true").await, vec!["pt-m2".to_owned()]);
    assert_eq!(found(&app, "/Patient?identifier:missing=false").await, vec!["pt-m1".to_owned()]);
}

#[tokio::test]
async fn the_not_modifier_excludes_every_matching_value() {
    let app = qualified().await;
    assert_eq!(found(&app, "/Patient?gender:not=male").await, vec!["pt-m1".to_owned()]);
    assert!(found(&app, "/Observation?status:not=final,registered").await.is_empty());
}

#[tokio::test]
async fn the_text_modifier_matches_the_narrative_of_a_code() {
    let app = qualified().await;
    assert_eq!(found(&app, "/Observation?code:text=temperature").await, vec!["ob-m1".to_owned()]);
    assert!(found(&app, "/Observation?code:text=survey").await.is_empty());
}

#[tokio::test]
async fn code_set_membership_is_resolved_by_the_store() {
    let app = qualified().await;
    assert_eq!(found(&app, "/Observation?code:in=http://x/vitals").await, vec!["ob-m1".to_owned()]);
    assert_eq!(found(&app, "/Observation?code:not-in=http://x/vitals").await, vec!["ob-m2".to_owned()]);
    let unknown = request(&app, "GET", "/Observation?code:in=http://x/none", &[], &[]).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn hierarchy_modifiers_walk_a_code() {
    let app = qualified().await;
    assert_eq!(found(&app, "/Observation?code:below=vital").await, vec!["ob-m1".to_owned()]);
    assert!(found(&app, "/Observation?code:below=other").await.is_empty());
    assert_eq!(
        found(&app, "/Observation?code:above=vital.temperature.core").await,
        vec!["ob-m1".to_owned()]
    );
}

#[tokio::test]
async fn reference_modifiers_read_the_type_and_the_identifier() {
    let app = qualified().await;
    assert_eq!(found(&app, "/Observation?subject:Patient=pt-m1").await, vec!["ob-m1".to_owned()]);
    assert!(found(&app, "/Observation?subject:Group=pt-m1").await.is_empty());
    assert_eq!(
        found(&app, "/Observation?subject:identifier=urn:mrn|12345").await,
        vec!["ob-m2".to_owned()]
    );
}

#[tokio::test]
async fn the_of_type_modifier_matches_a_qualified_identifier() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Patient?identifier:of-type=urn:t|MR|12345").await,
        vec!["pt-m1".to_owned()]
    );
    assert!(found(&app, "/Patient?identifier:of-type=urn:t|MR|999").await.is_empty());
}

#[tokio::test]
async fn a_modifier_the_parameter_forbids_is_rejected() {
    let app = qualified().await;
    for uri in ["/Patient?gender:exact=male", "/Patient?family:nonesuch=Ann"] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{uri}");
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported", "{uri}");
    }
    let reply = request(&app, "GET", "/Patient?identifier:missing=perhaps", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}
