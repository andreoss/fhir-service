use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, ResourceType, VersionId};
use fhir_store::{ResourceStore, SearchParams};
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
    async fn search(&self, _: Option<ResourceType>, _: &SearchParams) -> Result<Vec<ResourceEnvelope>, Error> {
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
