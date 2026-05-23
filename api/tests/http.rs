use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, VersionId};
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

fn writing(version: FhirVersion) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    Service::new(Arc::new(store), version, dependencies)
}

fn service() -> Service {
    writing(FhirVersion::R4)
}

async fn request(
    app: &Service,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
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
        check: Arc::new(|| Box::pin(async { Err("down".to_owned()) })),
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
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-5",
        &[("if-match", "W/\"1\"")],
        &patient("pt-5", false),
    )
    .await;
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
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-6",
        &[("if-match", "W/\"1\"")],
        &patient("pt-6", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
    assert_eq!(header(&reply, "last-modified"), LAST_MODIFIED);
    assert!(header(&reply, "location").ends_with("/Patient/pt-6/_history/2"));
    assert!(header(&reply, "content-location").ends_with("/Patient/pt-6/_history/2"));
}

#[tokio::test]
async fn a_stale_if_match_on_update_is_precondition_failed_and_writes_no_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-7", true)).await;
    request(
        &app,
        "PUT",
        "/Patient/pt-7",
        &[("if-match", "W/\"1\"")],
        &patient("pt-7", false),
    )
    .await;
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-7",
        &[("if-match", "W/\"1\"")],
        &patient("pt-7", true),
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        reply.body
    );
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "conflict");
    let current = request(&app, "GET", "/Patient/pt-7", &[], &[]).await;
    assert_eq!(header(&current, "etag"), "W/\"2\"");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&current.body).unwrap()["active"],
        false
    );
}

#[tokio::test]
async fn an_early_release_answers_a_stale_if_match_with_a_conflict() {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let app = Service::new(Arc::new(store), FhirVersion::Stu3, vec![]);
    request(&app, "POST", "/Patient", &[], &patient("pt-7s", true)).await;
    request(
        &app,
        "PUT",
        "/Patient/pt-7s",
        &[("if-match", "W/\"1\"")],
        &patient("pt-7s", false),
    )
    .await;
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-7s",
        &[("if-match", "W/\"1\"")],
        &patient("pt-7s", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
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
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-9",
        &[("if-match", "W/\"1\"")],
        &patient("pt-9", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        header(&reply, "etag"),
        "W/\"1\"",
        "a no-op update must not advance the version"
    );
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
async fn update_of_an_unknown_id_creates_the_version_the_statement_advertises() {
    let app = service();
    let statement = request(&app, "GET", "/metadata", &[], &[]).await;
    let advertised: serde_json::Value = serde_json::from_str(&statement.body).unwrap();
    let entry = advertised["rest"][0]["resource"]
        .as_array()
        .expect("the statement lists resources")
        .iter()
        .find(|held| held["type"] == "Patient")
        .expect("Patient is served");
    assert_eq!(
        entry["updateCreate"],
        serde_json::Value::Bool(true),
        "this test states the branch the statement advertises"
    );

    let reply = request(
        &app,
        "PUT",
        "/Patient/nobody",
        &[],
        &patient("nobody", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(header(&reply, "etag"), "W/\"1\"");
    assert!(header(&reply, "location").ends_with("/Patient/nobody/_history/1"));
    let read = request(&app, "GET", "/Patient/nobody", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&read.body).unwrap()["active"],
        true
    );
}

#[tokio::test]
async fn update_of_an_unknown_id_under_an_if_match_is_not_found() {
    let app = service();
    let reply = request(
        &app,
        "PUT",
        "/Patient/nobody-either",
        &[("if-match", "W/\"1\"")],
        &patient("nobody-either", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "not-found");
    let read = request(&app, "GET", "/Patient/nobody-either", &[], &[]).await;
    assert_eq!(
        read.status,
        StatusCode::NOT_FOUND,
        "no version may be written"
    );
}

#[tokio::test]
async fn invalid_if_match_header_is_rejected() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-11", true)).await;
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-11",
        &[("if-match", "\"1\"")],
        &patient("pt-11", true),
    )
    .await;
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
    let reply = request(&app, "GET", "/not/a/route/at/all", &[], &[]).await;
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
    assert_eq!(value["issue"][0]["code"], "forbidden");
}

struct FailingStore(fn() -> Error);

#[async_trait]
impl ResourceStore for FailingStore {
    async fn create(&self, _: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
        Err((self.0)())
    }
    async fn read(&self, _: &fhir_core::ResourceKey) -> Result<ResourceEnvelope, Error> {
        Err((self.0)())
    }
    async fn vread(
        &self,
        _: &fhir_core::ResourceKey,
        _: &VersionId,
    ) -> Result<ResourceEnvelope, Error> {
        Err((self.0)())
    }
    async fn update(
        &self,
        _: ResourceEnvelope,
        _: Option<&VersionId>,
    ) -> Result<ResourceEnvelope, Error> {
        Err((self.0)())
    }
    async fn search(&self, _: &SearchQuery) -> Result<SearchPage, Error> {
        Err((self.0)())
    }
    async fn delete(&self, _: &fhir_core::ResourceKey) -> Result<ResourceEnvelope, Error> {
        Err((self.0)())
    }
    async fn hard_delete(&self, _: &fhir_core::ResourceKey) -> Result<(), Error> {
        Err((self.0)())
    }
    async fn purge_history(&self, _: &fhir_core::ResourceKey) -> Result<usize, Error> {
        Err((self.0)())
    }
    async fn history(&self, _: &HistoryScope, _: &HistoryQuery) -> Result<HistoryPage, Error> {
        Err((self.0)())
    }
    async fn health(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[tokio::test]
async fn an_internal_failure_is_a_500_outcome_that_names_nothing_inside() {
    let app = Service::new(
        Arc::new(FailingStore(|| {
            Error::Internal("connection string dbuser@10.0.0.4 poisoned at row 7".to_owned())
        })),
        FhirVersion::R4,
        vec![],
    );
    for (method, uri) in [
        ("GET", "/Patient/boom"),
        ("GET", "/Patient/boom/_history/1"),
        ("GET", "/Patient"),
        ("GET", "/Patient/boom/_history"),
    ] {
        let reply = request(&app, method, uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{method} {uri}"
        );
        assert_eq!(header(&reply, "content-type"), "application/fhir+json");
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        let object = value.as_object().expect("outcome must be an object");
        assert_eq!(
            object.len(),
            2,
            "outcome must expose only resourceType and issue"
        );
        assert_eq!(value["resourceType"], "OperationOutcome");
        assert_eq!(value["issue"][0]["code"], "processing");
        for named in ["dbuser", "10.0.0.4", "poisoned", "row 7"] {
            assert!(
                !reply.body.contains(named),
                "{method} {uri} leaked {named}: {}",
                reply.body
            );
        }
    }
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
    assert_eq!(
        header(&reply, "etag"),
        "W/\"1\"",
        "the headers are those a create would have carried"
    );
    assert!(
        header(&reply, "location").ends_with("/Patient/pt-c2/_history/1"),
        "location was {}",
        header(&reply, "location")
    );
    assert_eq!(header(&reply, "last-modified"), LAST_MODIFIED);
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
    let reply = request(
        &app,
        "POST",
        "/Patient",
        &[("if-none-exist", "")],
        &patient("pt-c7", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn conditional_update_without_a_match_creates_the_resource() {
    let app = service();
    let reply = request(
        &app,
        "PUT",
        "/Patient?_id=pt-u1",
        &[],
        &patient("pt-u1", true),
    )
    .await;
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
    assert_eq!(
        reply.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        reply.body
    );
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "conflict");
    let read = request(&app, "GET", "/Patient/pt-u3", &[], &[]).await;
    assert_eq!(
        header(&read, "etag"),
        "W/\"1\"",
        "no version may be written"
    );
}

#[tokio::test]
async fn conditional_update_with_a_mismatched_body_id_is_rejected() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-u4", true)).await;
    let reply = request(
        &app,
        "PUT",
        "/Patient?_id=pt-u4",
        &[],
        &patient("pt-other", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn conditional_update_with_many_matches_is_412() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-u5", true)).await;
    request(&app, "POST", "/Patient", &[], &patient("pt-u6", true)).await;
    let reply = request(
        &app,
        "PUT",
        "/Patient?active=true",
        &[],
        &patient("pt-u5", false),
    )
    .await;
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
    let reply = request(
        &app,
        "PUT",
        "/Patient?_format=json",
        &[],
        &patient("pt-u8", true),
    )
    .await;
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
async fn deleting_a_resource_that_does_not_exist_is_no_content() {
    let app = service();
    let reply = request(&app, "DELETE", "/Patient/pt-none", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert!(reply.body.is_empty(), "{}", reply.body);
    let read = request(&app, "GET", "/Patient/pt-none", &[], &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND, "nothing may be created");
}

#[tokio::test]
async fn a_deleted_resource_is_restored_by_an_update() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d5", true)).await;
    request(&app, "DELETE", "/Patient/pt-d5", &[], &[]).await;
    let reply = request(&app, "PUT", "/Patient/pt-d5", &[], &patient("pt-d5", false)).await;
    assert_eq!(
        reply.status,
        StatusCode::CREATED,
        "the specification asks for 201 when a deleted resource is brought back to life"
    );
    assert_eq!(header(&reply, "etag"), "W/\"3\"");
    assert!(header(&reply, "location").ends_with("/Patient/pt-d5/_history/3"));
    let read = request(&app, "GET", "/Patient/pt-d5", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&read.body).unwrap()["active"],
        false
    );
    let marker = request(&app, "GET", "/Patient/pt-d5/_history/2", &[], &[]).await;
    assert_eq!(marker.status, StatusCode::GONE);
}

#[tokio::test]
async fn an_update_under_the_etag_of_the_delete_brings_the_resource_back() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d9", true)).await;
    let deleted = request(&app, "DELETE", "/Patient/pt-d9", &[], &[]).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let marker = header(&deleted, "etag").to_owned();
    assert_eq!(marker, "W/\"2\"", "the delete names the version it wrote");
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-d9",
        &[("if-match", &marker)],
        &patient("pt-d9", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(header(&reply, "etag"), "W/\"3\"");
    let read = request(&app, "GET", "/Patient/pt-d9", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK);
}

#[tokio::test]
async fn a_stale_if_match_brings_nothing_back_and_writes_no_version() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d10", true)).await;
    request(&app, "DELETE", "/Patient/pt-d10", &[], &[]).await;
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-d10",
        &[("if-match", "W/\"1\"")],
        &patient("pt-d10", false),
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        reply.body
    );
    let read = request(&app, "GET", "/Patient/pt-d10", &[], &[]).await;
    assert_eq!(read.status, StatusCode::GONE, "{}", read.body);
    let version = request(&app, "GET", "/Patient/pt-d10/_history/3", &[], &[]).await;
    assert_eq!(version.status, StatusCode::NOT_FOUND, "nothing was written");
}

#[tokio::test]
async fn an_update_of_a_live_resource_answers_ok() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-d11", true)).await;
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-d11",
        &[],
        &patient("pt-d11", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
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
async fn conditional_delete_without_a_match_is_no_content() {
    let app = service();
    let reply = request(&app, "DELETE", "/Patient?_id=pt-none", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert!(reply.body.is_empty(), "{}", reply.body);
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
    let reply = request(
        &app,
        "DELETE",
        "/Patient?_id=pt-db&_hardDelete=true",
        &[],
        &[],
    )
    .await;
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
        let reply = request(
            &app,
            "PATCH",
            "/Patient/pt-p4",
            &[("content-type", JSON_PATCH)],
            &patch,
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "body was {}",
            reply.body
        );
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
    assert_eq!(
        reply.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        reply.body
    );
    let read = request(&app, "GET", "/Patient/pt-p5", &[], &[]).await;
    assert_eq!(
        header(&read, "etag"),
        "W/\"1\"",
        "no version may be written"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&read.body).unwrap()["active"],
        true
    );
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
    let none = request(
        &app,
        "PATCH",
        "/Patient?_id=pt-none",
        &[("content-type", JSON_PATCH)],
        patch,
    )
    .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    let many = request(
        &app,
        "PATCH",
        "/Patient?active=true",
        &[("content-type", JSON_PATCH)],
        patch,
    )
    .await;
    assert_eq!(many.status, StatusCode::PRECONDITION_FAILED);
    let empty = request(
        &app,
        "PATCH",
        "/Patient",
        &[("content-type", JSON_PATCH)],
        patch,
    )
    .await;
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
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
}

async fn seeded_history() -> Service {
    let app = ticking_service();
    request(&app, "POST", "/Patient", &[], &patient("pt-h1", true)).await;
    request(&app, "PUT", "/Patient/pt-h1", &[], &patient("pt-h1", false)).await;
    request(&app, "DELETE", "/Patient/pt-h1", &[], &[]).await;
    request(
        &app,
        "POST",
        "/Observation",
        &[],
        br#"{"resourceType":"Observation","id":"ob-h1","status":"final","code":{"text":"probe"}}"#,
    )
    .await;
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
                .map(|entry| {
                    entry["response"]["etag"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
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
    assert!(
        value["entry"][0]["resource"].is_null(),
        "a delete marker carries no resource"
    );
    assert_eq!(value["entry"][1]["request"]["method"], "PUT");
    assert_eq!(value["entry"][1]["request"]["url"], "Patient/pt-h1");
    assert_eq!(value["entry"][1]["response"]["status"], "200");
    assert_eq!(value["entry"][1]["resource"]["active"], false);
    assert_eq!(value["entry"][2]["request"]["method"], "POST");
    assert_eq!(value["entry"][2]["request"]["url"], "Patient");
    assert_eq!(value["entry"][2]["response"]["status"], "201");
    assert_eq!(
        value["entry"][2]["fullUrl"],
        "http://localhost/Patient/pt-h1"
    );
    assert_eq!(
        value["entry"][2]["response"]["lastModified"],
        "2026-09-06T04:00:00Z"
    );
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
    let since = bundle(
        &request(
            &app,
            "GET",
            "/_history?_since=2026-09-06T04:00:02Z",
            &[],
            &[],
        )
        .await,
    );
    assert_eq!(since["total"], 2);
    let before = bundle(
        &request(
            &app,
            "GET",
            "/_history?_before=2026-09-06T04:00:01Z",
            &[],
            &[],
        )
        .await,
    );
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
    let value = bundle(
        &request(
            &app,
            "GET",
            "/Patient/pt-h1/_history?_sort=_lastUpdated",
            &[],
            &[],
        )
        .await,
    );
    assert_eq!(entry_versions(&value), ["W/\"1\"", "W/\"2\"", "W/\"3\""]);
    let reverse = bundle(
        &request(
            &app,
            "GET",
            "/Patient/pt-h1/_history?_sort=-_lastUpdated",
            &[],
            &[],
        )
        .await,
    );
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
    let value = bundle(
        &request(
            &app,
            "GET",
            "/Patient/pt-h1/_history?_summary=true",
            &[],
            &[],
        )
        .await,
    );
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
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{query} must be rejected"
        );
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
                .map(|item| {
                    item["resource"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
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
    let recent =
        bundle(&request(&app, "GET", "/Patient?_lastUpdated=ge2026-09-06", &[], &[]).await);
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
    for uri in [
        "/Patient?nonesuch=1",
        "/Patient?_include=Patient:nonesuch",
        "/Patient?_content=x",
        "/Patient?_type=Patient",
    ] {
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
        request(
            &app,
            "POST",
            "/Patient",
            &[],
            &patient(&format!("pt-r{index}"), true),
        )
        .await;
    }
    app
}

#[tokio::test]
async fn count_pages_the_result_and_offers_a_next_link() {
    let app = many().await;
    let first = bundle(&request(&app, "GET", "/Patient?_count=2", &[], &[]).await);
    assert_eq!(first["total"], 5);
    assert_eq!(
        entries(&first),
        vec!["pt-r1".to_owned(), "pt-r2".to_owned()]
    );
    let next = link(&first, "next");
    assert!(next.contains("ct="), "next was {next}");
    let token = next.rsplit("ct=").next().unwrap().to_owned();
    let second = bundle(
        &request(
            &app,
            "GET",
            &format!("/Patient?_count=2&ct={token}"),
            &[],
            &[],
        )
        .await,
    );
    assert_eq!(
        entries(&second),
        vec!["pt-r3".to_owned(), "pt-r4".to_owned()]
    );
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
    assert_eq!(
        bundle(&request(&app, "GET", "/Patient?_total=accurate", &[], &[]).await)["total"],
        5
    );
    assert_eq!(
        bundle(&request(&app, "GET", "/Patient?_total=estimate", &[], &[]).await)["total"],
        5
    );
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
    for uri in [
        "/Patient?_format=json",
        "/Patient?_format=application/fhir%2Bjson",
    ] {
        assert_eq!(
            request(&app, "GET", uri, &[], &[]).await.status,
            StatusCode::OK,
            "{uri}"
        );
    }
    let reply = request(&app, "GET", "/Patient?_format=xml", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
        header(&reply, "content-type"),
        "application/fhir+xml",
        "{}",
        reply.body
    );
    let text = reply.body;
    assert!(text.starts_with("<Bundle"), "{text}");
    let reply = request(&app, "GET", "/Patient?_format=yaml", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_ACCEPTABLE, "{}", reply.body);
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
    let rate = br#"{"resourceType":"Observation","id":"ob-v1","status":"final","code":{"text":"probe"},"code":{"coding":[{"system":"http://loinc.org","code":"8867-4"}]},"subject":{"reference":"Patient/pt-v1"},"effectiveDateTime":"2026-09-06T04:00:00Z","valueQuantity":{"value":72.5,"system":"http://unitsofmeasure.org","code":"/min"}}"#;
    let pressure = br#"{"resourceType":"Observation","id":"ob-v2","status":"final","code":{"text":"probe"},"code":{"coding":[{"system":"http://loinc.org","code":"85354-9"}]},"subject":{"reference":"Patient/pt-v2"},"effectiveDateTime":"2026-09-06T04:00:00Z","component":[{"code":{"coding":[{"system":"http://loinc.org","code":"8480-6"}]},"valueQuantity":{"value":120,"system":"http://unitsofmeasure.org","code":"mm[Hg]"}}]}"#;
    request(&app, "POST", "/Observation", &[], rate).await;
    request(&app, "POST", "/Observation", &[], pressure).await;
    let risk = br#"{"resourceType":"RiskAssessment","id":"ra-v1","status":"final","subject":{"reference":"Patient/pt-v1"},"prediction":[{"probabilityDecimal":0.42}]}"#;
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
    assert_eq!(
        found(&app, "/Patient?family=de%20la").await,
        vec!["pt-v1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Patient?given=BO").await,
        vec!["pt-v2".to_owned()]
    );
    assert_eq!(
        found(&app, "/Patient?birthdate=lt1990").await,
        vec!["pt-v1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Patient?birthdate=1995-11-20").await,
        vec!["pt-v2".to_owned()]
    );
}

#[tokio::test]
async fn a_search_matches_token_reference_and_uri_values() {
    let app = clinical().await;
    assert_eq!(
        found(&app, "/Observation?code=http://loinc.org|8867-4").await,
        vec!["ob-v1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?subject=Patient/pt-v2").await,
        vec!["ob-v2".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?patient=pt-v1").await,
        vec!["ob-v1".to_owned()]
    );
    assert_eq!(found(&app, "/Observation?status=final").await.len(), 2);
    assert_eq!(
        found(&app, "/Patient?organization=Organization/org-1").await,
        vec!["pt-v1".to_owned()]
    );
}

#[tokio::test]
async fn a_search_matches_quantity_number_and_composite_values() {
    let app = clinical().await;
    assert_eq!(
        found(
            &app,
            "/Observation?value-quantity=72.5|http://unitsofmeasure.org|/min"
        )
        .await,
        vec!["ob-v1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?value-quantity=gt70").await,
        vec!["ob-v1".to_owned()]
    );
    assert!(found(&app, "/Observation?value-quantity=lt70")
        .await
        .is_empty());
    assert_eq!(
        found(&app, "/RiskAssessment?probability=0.42").await,
        vec!["ra-v1".to_owned()]
    );
    assert_eq!(
        found(
            &app,
            "/Observation?component-code-value-quantity=8480-6$120"
        )
        .await,
        vec!["ob-v2".to_owned()]
    );
    assert!(found(
        &app,
        "/Observation?component-code-value-quantity=8867-4$120"
    )
    .await
    .is_empty());
}

#[tokio::test]
async fn a_malformed_composite_value_is_rejected() {
    let app = clinical().await;
    let reply = request(
        &app,
        "GET",
        "/Observation?component-code-value-quantity=8480-6",
        &[],
        &[],
    )
    .await;
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
    let ann = br#"{"resourceType":"Patient","id":"pt-m1","name":[{"family":"Sorensen","given":["Ann"]}],"gender":"female","managingOrganization":{"reference":"Organization/org-m1"},"identifier":[{"type":{"coding":[{"system":"urn:t","code":"MR"}]},"system":"urn:mrn","value":"12345"}]}"#;
    let clinic = br#"{"resourceType":"Organization","id":"org-m1","name":"Mercy","active":true}"#;
    request(&app, "POST", "/Organization", &[], clinic).await;
    let bo =
        br#"{"resourceType":"Patient","id":"pt-m2","name":[{"family":"Okonkwo"}],"gender":"male"}"#;
    request(&app, "POST", "/Patient", &[], ann).await;
    request(&app, "POST", "/Patient", &[], bo).await;
    let warm = br#"{"resourceType":"Observation","id":"ob-m1","status":"final","code":{"text":"probe"},"code":{"text":"Body Temperature","coding":[{"system":"urn:s","code":"vital.temperature"}]},"subject":{"reference":"Patient/pt-m1"}}"#;
    let other = br#"{"resourceType":"Observation","id":"ob-m2","status":"registered","code":{"text":"probe"},"code":{"coding":[{"system":"urn:s","code":"survey"}]},"subject":{"identifier":{"system":"urn:mrn","value":"12345"}}}"#;
    request(&app, "POST", "/Observation", &[], warm).await;
    request(&app, "POST", "/Observation", &[], other).await;
    let set = br#"{"resourceType":"ValueSet","id":"vs-m1","url":"http://x/vitals","status":"active","compose":{"include":[{"system":"urn:s","concept":[{"code":"vital.temperature"}]}]}}"#;
    request(&app, "POST", "/ValueSet", &[], set).await;
    app
}

#[tokio::test]
async fn string_modifiers_narrow_a_search() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Patient?family:exact=Okonkwo").await,
        vec!["pt-m2".to_owned()]
    );
    assert!(found(&app, "/Patient?family:exact=okonkwo")
        .await
        .is_empty());
    assert_eq!(
        found(&app, "/Patient?family:contains=oren").await,
        vec!["pt-m1".to_owned()]
    );
}

#[tokio::test]
async fn the_missing_modifier_selects_by_presence() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Patient?identifier:missing=true").await,
        vec!["pt-m2".to_owned()]
    );
    assert_eq!(
        found(&app, "/Patient?identifier:missing=false").await,
        vec!["pt-m1".to_owned()]
    );
}

#[tokio::test]
async fn the_not_modifier_excludes_every_matching_value() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Patient?gender:not=male").await,
        vec!["pt-m1".to_owned()]
    );
    assert!(found(&app, "/Observation?status:not=final,registered")
        .await
        .is_empty());
}

#[tokio::test]
async fn the_text_modifier_matches_the_narrative_of_a_code() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Observation?code:text=temperature").await,
        vec!["ob-m1".to_owned()]
    );
    assert!(found(&app, "/Observation?code:text=survey")
        .await
        .is_empty());
}

#[tokio::test]
async fn code_set_membership_is_resolved_by_the_store() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Observation?code:in=http://x/vitals").await,
        vec!["ob-m1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?code:not-in=http://x/vitals").await,
        vec!["ob-m2".to_owned()]
    );
    let unknown = request(&app, "GET", "/Observation?code:in=http://x/none", &[], &[]).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_subsumption_modifier_needs_a_code_system_that_defines_the_code() {
    let app = qualified().await;
    let system = br#"{"resourceType":"CodeSystem","id":"cs-m1","url":"urn:s","status":"active","content":"complete","hierarchyMeaning":"is-a","concept":[{"code":"vital","concept":[{"code":"vital.temperature"}]}]}"#;
    request(&app, "POST", "/CodeSystem", &[], system).await;
    assert_eq!(
        found(&app, "/Observation?code:below=urn:s%7Cvital").await,
        vec!["ob-m1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?code:above=urn:s%7Cvital.temperature").await,
        vec!["ob-m1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?code:below=vital").await,
        vec!["ob-m1".to_owned()],
        "a code with no system is resolved against every system the server holds"
    );
    for uri in [
        "/Observation?code:below=urn:s%7Cnonesuch",
        "/Observation?code:above=vital.temperature.core",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported", "{uri}");
    }
}

#[tokio::test]
async fn reference_modifiers_read_the_type_and_the_identifier() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Observation?subject:Patient=pt-m1").await,
        vec!["ob-m1".to_owned()]
    );
    assert!(found(&app, "/Observation?subject:Group=pt-m1")
        .await
        .is_empty());
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
    assert!(found(&app, "/Patient?identifier:of-type=urn:t|MR|999")
        .await
        .is_empty());
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

#[tokio::test]
async fn a_chained_search_follows_a_reference() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Observation?subject:Patient.family:exact=Sorensen").await,
        vec!["ob-m1".to_owned()]
    );
    assert_eq!(
        found(&app, "/Observation?patient.gender=female").await,
        vec!["ob-m1".to_owned()]
    );
    assert!(found(&app, "/Observation?patient.gender=male")
        .await
        .is_empty());
}

#[tokio::test]
async fn a_chain_reaches_across_more_than_one_link() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Observation?patient.organization.name=Mercy").await,
        vec!["ob-m1".to_owned()]
    );
    assert!(found(&app, "/Observation?patient.organization.name=Other")
        .await
        .is_empty());
}

#[tokio::test]
async fn a_reverse_chain_selects_by_what_points_at_the_resource() {
    let app = qualified().await;
    assert_eq!(
        found(&app, "/Patient?_has:Observation:patient:status=final").await,
        vec!["pt-m1".to_owned()]
    );
    assert!(
        found(&app, "/Patient?_has:Observation:patient:status=cancelled")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn reverse_chains_nest() {
    let app = qualified().await;
    let uri = "/Organization?_has:Patient:organization:_has:Observation:patient:status=final";
    assert_eq!(found(&app, uri).await, vec!["org-m1".to_owned()]);
    let none = "/Organization?_has:Patient:organization:_has:Observation:patient:status=amended";
    assert!(found(&app, none).await.is_empty());
}

#[tokio::test]
async fn a_reverse_chain_carries_a_forward_chain() {
    let app = qualified().await;
    let uri = "/Patient?_has:Observation:patient:patient.gender=female";
    assert_eq!(found(&app, uri).await, vec!["pt-m1".to_owned()]);
}

#[tokio::test]
async fn a_chain_the_server_cannot_follow_is_rejected() {
    let app = qualified().await;
    for uri in [
        "/Observation?subject.nonesuch=x",
        "/Observation?status.name=x",
        "/Patient?_has:Observation:nonesuch:status=final",
        "/Patient?_has:Observation:patient=final",
        "/Patient?_has:Nonesuch:patient:status=final",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
    }
}

fn by_mode(value: &serde_json::Value, mode: &str) -> Vec<String> {
    value["entry"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["search"]["mode"] == mode)
                .map(|item| {
                    item["resource"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn page(app: &Service, uri: &str) -> serde_json::Value {
    let reply = request(app, "GET", uri, &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri} gave {}", reply.body);
    bundle(&reply)
}

#[tokio::test]
async fn an_include_pulls_the_referenced_resource_in() {
    let app = qualified().await;
    let value = page(&app, "/Observation?_id=ob-m1&_include=Observation:subject").await;
    assert_eq!(value["total"], 1);
    assert_eq!(by_mode(&value, "match"), vec!["ob-m1".to_owned()]);
    assert_eq!(by_mode(&value, "include"), vec!["pt-m1".to_owned()]);
}

#[tokio::test]
async fn an_iterating_include_follows_what_it_has_pulled_in() {
    let app = qualified().await;
    let uri =
        "/Observation?_id=ob-m1&_include=Observation:subject&_include:iterate=Patient:organization";
    let value = page(&app, uri).await;
    let mut included = by_mode(&value, "include");
    included.sort();
    assert_eq!(included, vec!["org-m1".to_owned(), "pt-m1".to_owned()]);
    let once = page(
        &app,
        "/Observation?_id=ob-m1&_include=Observation:subject&_include=Patient:organization",
    )
    .await;
    assert_eq!(by_mode(&once, "include"), vec!["pt-m1".to_owned()]);
}

#[tokio::test]
async fn a_wildcard_include_follows_every_reference() {
    let app = qualified().await;
    let typed = page(&app, "/Observation?_id=ob-m1&_include=Observation:*").await;
    assert_eq!(by_mode(&typed, "include"), vec!["pt-m1".to_owned()]);
    let any = page(&app, "/Observation?_id=ob-m1&_include=*").await;
    assert_eq!(by_mode(&any, "include"), vec!["pt-m1".to_owned()]);
}

#[tokio::test]
async fn a_reverse_include_pulls_what_points_at_the_match() {
    let app = qualified().await;
    let value = page(&app, "/Patient?_id=pt-m1&_revinclude=Observation:patient").await;
    assert_eq!(by_mode(&value, "match"), vec!["pt-m1".to_owned()]);
    assert_eq!(by_mode(&value, "include"), vec!["ob-m1".to_owned()]);
}

#[tokio::test]
async fn includes_apply_to_the_page_and_never_to_the_total() {
    let app = qualified().await;
    let value = page(
        &app,
        "/Observation?_count=1&_sort=_id&_include=Observation:subject",
    )
    .await;
    assert_eq!(value["total"], 2);
    assert_eq!(by_mode(&value, "match"), vec!["ob-m1".to_owned()]);
    assert_eq!(by_mode(&value, "include"), vec!["pt-m1".to_owned()]);
    assert!(link(&value, "next").contains("ct="));
}

#[tokio::test]
async fn an_include_the_server_cannot_follow_is_rejected() {
    let app = qualified().await;
    for uri in [
        "/Observation?_include=Nonesuch:subject",
        "/Observation?_include=Observation:status",
        "/Observation?_include=Observation:nonesuch",
        "/Observation?_include:sideways=Observation:subject",
        "/Observation?_include=Observation",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn a_compartment_search_returns_what_belongs_to_the_resource() {
    let app = qualified().await;
    let value = page(&app, "/Patient/pt-m1/Observation").await;
    assert_eq!(by_mode(&value, "match"), vec!["ob-m1".to_owned()]);
    assert_eq!(value["total"], 1);
    let empty = page(&app, "/Patient/pt-m2/Observation").await;
    assert_eq!(empty["total"], 0);
}

#[tokio::test]
async fn a_compartment_search_narrows_further_by_query() {
    let app = qualified().await;
    let value = page(&app, "/Patient/pt-m1/Observation?status=final").await;
    assert_eq!(by_mode(&value, "match"), vec!["ob-m1".to_owned()]);
    let none = page(&app, "/Patient/pt-m1/Observation?status=cancelled").await;
    assert_eq!(none["total"], 0);
}

#[tokio::test]
async fn a_wildcard_compartment_gathers_every_type_it_covers() {
    let app = qualified().await;
    let value = page(&app, "/Patient/pt-m1/*").await;
    let mut ids = by_mode(&value, "match");
    ids.sort();
    assert_eq!(ids, vec!["ob-m1".to_owned(), "pt-m1".to_owned()]);
}

#[tokio::test]
async fn a_compartment_gathers_by_the_references_its_definition_names() {
    let app = qualified().await;
    let device = br#"{"resourceType":"Device","id":"dev-m1","status":"active"}"#;
    request(&app, "POST", "/Device", &[], device).await;
    let reading = br#"{"resourceType":"Observation","id":"ob-m3","status":"final","code":{"coding":[{"system":"urn:s","code":"survey"}]},"subject":{"reference":"Patient/pt-m1"},"device":{"reference":"Device/dev-m1"},"performer":[{"reference":"Patient/pt-m2"}]}"#;
    request(&app, "POST", "/Observation", &[], reading).await;

    let by_device = page(&app, "/Device/dev-m1/Observation").await;
    assert_eq!(by_mode(&by_device, "match"), vec!["ob-m3".to_owned()]);

    let by_performer = page(&app, "/Patient/pt-m2/Observation").await;
    assert_eq!(
        by_mode(&by_performer, "match"),
        vec!["ob-m3".to_owned()],
        "a patient who is only the performer is still in their own compartment"
    );
}

#[tokio::test]
async fn a_compartment_the_server_does_not_define_is_rejected() {
    let app = qualified().await;
    for uri in [
        "/Observation/ob-m1/Patient",
        "/Patient/pt-m1/Organization",
        "/Organization/org-m1/Patient",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported");
    }
}

#[tokio::test]
async fn the_compartment_definitions_are_served() {
    let app = qualified().await;
    let listing = page(&app, "/CompartmentDefinition").await;
    assert_eq!(listing["type"], "searchset");
    assert!(listing["total"].as_u64().is_some_and(|total| total >= 4));
    let one = page(&app, "/CompartmentDefinition/Patient").await;
    assert_eq!(one["resourceType"], "CompartmentDefinition");
    assert_eq!(one["code"], "Patient");
    assert_eq!(one["search"], true);
    let missing = request(&app, "GET", "/CompartmentDefinition/Nonesuch", &[], &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

async fn next_token(app: &Service, uri: &str) -> String {
    let value = page(app, uri).await;
    let next = link(&value, "next");
    assert!(next.contains("ct="), "next was {next}");
    next.rsplit("ct=").next().unwrap().to_owned()
}

#[tokio::test]
async fn a_continuation_token_is_opaque_and_carries_no_offset() {
    let app = qualified().await;
    let token = next_token(&app, "/Patient?_count=1&_sort=_id").await;
    assert_eq!(token.len(), 32);
    assert!(token.chars().all(|found| found.is_ascii_hexdigit()));
    assert_ne!(&token[16..], format!("{:016x}", 1u64));
}

#[tokio::test]
async fn a_continuation_token_belongs_to_one_query_only() {
    let app = qualified().await;
    let token = next_token(&app, "/Patient?_count=1&_sort=_id").await;
    let same = page(&app, &format!("/Patient?_count=1&_sort=_id&ct={token}")).await;
    assert_eq!(by_mode(&same, "match"), vec!["pt-m2".to_owned()]);
    let other = request(
        &app,
        "GET",
        &format!("/Patient?_count=1&_sort=-_id&ct={token}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(other.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&other.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "invalid");
}

#[tokio::test]
async fn an_edited_continuation_token_is_refused() {
    let app = qualified().await;
    let token = next_token(&app, "/Patient?_count=1&_sort=_id").await;
    let last = token.chars().last().unwrap_or('0');
    let flipped = if last == '0' { '1' } else { '0' };
    let edited = format!("{}{flipped}", &token[..token.len() - 1]);
    let reply = request(
        &app,
        "GET",
        &format!("/Patient?_count=1&_sort=_id&ct={edited}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_compartment_page_carries_its_own_token() {
    let app = qualified().await;
    let token = next_token(&app, "/Patient/pt-m1/*?_count=1&_sort=_id").await;
    let second = page(
        &app,
        &format!("/Patient/pt-m1/*?_count=1&_sort=_id&ct={token}"),
    )
    .await;
    assert_eq!(by_mode(&second, "match"), vec!["pt-m1".to_owned()]);
}

#[tokio::test]
async fn a_parameter_spelled_without_a_value_is_never_dropped() {
    let app = seeded().await;
    for uri in [
        "/Patient?nonesuch",
        "/Patient?_summary",
        "/Patient?_count",
        "/Patient?ct",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn an_unknown_parameter_never_reaches_the_store() {
    let app = seeded().await;
    for uri in [
        "/Patient?_filter=name%20eq%20Ann",
        "/Patient?_query=byName",
        "/Patient?name:nonesuch=Ann",
        "/Patient?_has:Observation:patient:nonesuch=1",
        "/Patient?nonesuch.name=Ann",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported", "{uri}");
    }
}

async fn granted() -> Service {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        br#"{"resourceType":"Patient","id":"pt-g1","active":true}"#,
    )
    .await;
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        br#"{"resourceType":"Patient","id":"pt-g2","active":true}"#,
    )
    .await;
    let mine = br#"{"resourceType":"Observation","id":"ob-g1","status":"final","code":{"text":"probe"},"subject":{"reference":"Patient/pt-g1"}}"#;
    let other = br#"{"resourceType":"Observation","id":"ob-g2","status":"final","code":{"text":"probe"},"subject":{"reference":"Patient/pt-g2"}}"#;
    request(&app, "POST", "/Observation", &[], mine).await;
    request(&app, "POST", "/Observation", &[], other).await;
    app
}

async fn scoped(app: &Service, uri: &str, grant: &str) -> serde_json::Value {
    let reply = request(app, "GET", uri, &[("x-scope", grant)], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri} gave {}", reply.body);
    bundle(&reply)
}

#[tokio::test]
async fn a_grant_confines_matches_to_its_compartment() {
    let app = granted().await;
    let value = scoped(&app, "/Observation", "compartment=Patient/pt-g1").await;
    assert_eq!(by_mode(&value, "match"), vec!["ob-g1".to_owned()]);
    assert_eq!(value["total"], 1);
    let outside = scoped(
        &app,
        "/Observation?patient=pt-g2",
        "compartment=Patient/pt-g1",
    )
    .await;
    assert_eq!(outside["total"], 0);
}

#[tokio::test]
async fn a_grant_confines_the_resources_an_include_pulls_in() {
    let app = granted().await;
    let value = scoped(
        &app,
        "/Observation?_include=Observation:subject",
        "types=Observation",
    )
    .await;
    assert!(by_mode(&value, "include").is_empty(), "{value}");
    let allowed = scoped(
        &app,
        "/Observation?_include=Observation:subject",
        "types=Observation,Patient;compartment=Patient/pt-g1",
    )
    .await;
    assert_eq!(by_mode(&allowed, "include"), vec!["pt-g1".to_owned()]);
}

#[tokio::test]
async fn a_grant_confines_what_a_chain_can_reach() {
    let app = granted().await;
    let value = scoped(
        &app,
        "/Patient?_has:Observation:patient:_id=ob-g2",
        "compartment=Patient/pt-g1",
    )
    .await;
    assert_eq!(value["total"], 0);
    let own = scoped(
        &app,
        "/Patient?_has:Observation:patient:_id=ob-g1",
        "compartment=Patient/pt-g1",
    )
    .await;
    assert_eq!(by_mode(&own, "match"), vec!["pt-g1".to_owned()]);
}

#[tokio::test]
async fn a_type_outside_the_grant_is_refused() {
    let app = granted().await;
    let reply = request(
        &app,
        "GET",
        "/Observation",
        &[("x-scope", "types=Patient")],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "forbidden");
}

#[tokio::test]
async fn a_malformed_grant_is_rejected() {
    let app = granted().await;
    let reply = request(&app, "GET", "/Patient", &[("x-scope", "nonesuch=1")], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
}

#[tokio::test]
async fn a_token_longer_than_the_index_key_matches_exactly() {
    let app = service();
    let value = "u".repeat(600);
    let body = format!(
        r#"{{"resourceType":"Patient","id":"pt-t1","identifier":[{{"system":"urn:mrn","value":"{value}-a"}}]}}"#
    );
    request(&app, "POST", "/Patient", &[], body.as_bytes()).await;
    let hit = format!("/Patient?identifier=urn:mrn|{value}-a");
    assert_eq!(found(&app, &hit).await, vec!["pt-t1".to_owned()]);
    let miss = format!("/Patient?identifier=urn:mrn|{value}-b");
    assert!(found(&app, &miss).await.is_empty());
}

#[tokio::test]
async fn a_full_text_parameter_is_reported_unsupported() {
    let app = seeded().await;
    for uri in [
        "/Patient?_content=fever",
        "/Patient?_content:exact=fever",
        "/Patient?_query=fever",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported", "{uri}");
    }
}

#[tokio::test]
async fn a_full_text_search_matches_the_words_of_the_narrative() {
    let app = service();
    let report = br#"{"resourceType":"Observation","id":"ob-t1","status":"final","text":{"status":"generated","div":"<div><p>The patient reported a <b>high fever</b> and chills, with bone pain</p></div>"},"code":{"text":"report"}}"#;
    let plain = br#"{"resourceType":"Observation","id":"ob-t2","status":"final","text":{"status":"generated","div":"<div><p>An unrelated follow-up note</p></div>"},"code":{"text":"report"}}"#;
    request(&app, "POST", "/Observation", &[], report).await;
    request(&app, "POST", "/Observation", &[], plain).await;
    assert_eq!(
        found(&app, "/Observation?_text=fever").await,
        vec!["ob-t1".to_owned()]
    );
    assert!(found(&app, "/Observation?_text=rash").await.is_empty());
}

#[tokio::test]
async fn a_full_text_search_is_word_based() {
    let app = service();
    let feverish = br#"{"resourceType":"Observation","id":"ob-t3","status":"final","text":{"status":"generated","div":"<div><p>The patient felt feverish overnight</p></div>"},"code":{"text":"report"}}"#;
    request(&app, "POST", "/Observation", &[], feverish).await;
    assert!(found(&app, "/Observation?_text=fever").await.is_empty());
    assert_eq!(
        found(&app, "/Observation?_text=feverish").await,
        vec!["ob-t3".to_owned()]
    );
}

#[tokio::test]
async fn a_full_text_search_reads_a_boolean_expression() {
    let app = service();
    let liver = br#"{"resourceType":"Observation","id":"ob-t4","status":"final","text":{"status":"generated","div":"<div><p>Metastases in the liver</p></div>"},"code":{"text":"report"}}"#;
    let bone = br#"{"resourceType":"Observation","id":"ob-t5","status":"final","text":{"status":"generated","div":"<div><p>Bone metastases found</p></div>"},"code":{"text":"report"}}"#;
    let none = br#"{"resourceType":"Observation","id":"ob-t6","status":"final","text":{"status":"generated","div":"<div><p>A routine check</p></div>"},"code":{"text":"report"}}"#;
    request(&app, "POST", "/Observation", &[], liver).await;
    request(&app, "POST", "/Observation", &[], bone).await;
    request(&app, "POST", "/Observation", &[], none).await;
    let query = "/Observation?_text=(bone%20OR%20liver)%20AND%20metastases";
    let mut entries = found(&app, query).await;
    entries.sort();
    assert_eq!(entries, vec!["ob-t4".to_owned(), "ob-t5".to_owned()]);
    assert!(found(&app, "/Observation?_text=bone%20AND%20liver")
        .await
        .is_empty());
    assert!(!found(&app, "/Observation?_text=bone%20AND%20metastases")
        .await
        .is_empty());
}

#[tokio::test]
async fn the_text_modifier_on_a_reference_is_answered_in_r5_only() {
    let body = br#"{"resourceType":"Patient","id":"pt-r1","managingOrganization":{"reference":"Organization/org-r1","display":"Mercy General Hospital"}}"#;
    let older = writing(FhirVersion::R4);
    request(&older, "POST", "/Patient", &[], body).await;
    let reply = request(&older, "GET", "/Patient?organization:text=mercy", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let latest = writing(FhirVersion::R5);
    request(&latest, "POST", "/Patient", &[], body).await;
    assert_eq!(
        found(&latest, "/Patient?organization:text=mercy").await,
        vec!["pt-r1".to_owned()]
    );
    assert!(found(&latest, "/Patient?organization:text=district")
        .await
        .is_empty());
}

#[tokio::test]
async fn an_empty_value_is_reported_unsupported() {
    let app = seeded().await;
    for uri in [
        "/Patient?_id=",
        "/Patient?name=",
        "/Patient?_sort=",
        "/Patient?_count=",
        "/Patient?_tag=",
        "/Patient?_text=",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{uri} gave {}",
            reply.body
        );
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["issue"][0]["code"], "not-supported", "{uri}");
    }
}

fn definition(id: &str, code: &str, expression: &str, status: &str) -> Vec<u8> {
    format!(
        r#"{{"resourceType":"SearchParameter","id":"{id}","name":"{code}","description":"a parameter","url":"urn:p:{code}","status":"{status}","code":"{code}","base":["Patient"],"type":"token","expression":"{expression}"}}"#
    )
    .into_bytes()
}

async fn diagnostics(app: &Service, uri: &str) -> String {
    let reply = request(app, "GET", uri, &[], &[]).await;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "{uri} gave {}",
        reply.body
    );
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    value["issue"][0]["diagnostics"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn a_definition_registers_a_custom_parameter() {
    let app = service();
    assert!(!diagnostics(&app, "/Patient?risk-band=high")
        .await
        .contains("is supported"));
    let body = definition("sp-1", "risk-band", "Patient.extension.valueCode", "active");
    let reply = request(&app, "POST", "/SearchParameter", &[], &body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert!(diagnostics(&app, "/Patient?risk-band=high")
        .await
        .contains("is supported"));
}

#[tokio::test]
async fn a_malformed_definition_registers_nothing() {
    let app = service();
    let body = br#"{"resourceType":"SearchParameter","id":"sp-2","name":"bad","description":"a parameter","url":"urn:p:bad","status":"active","code":"bad","base":["Patient"],"type":"token"}"#;
    let reply = request(&app, "POST", "/SearchParameter", &[], body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(!diagnostics(&app, "/Patient?bad=1")
        .await
        .contains("is supported"));
    let read = request(&app, "GET", "/SearchParameter/sp-2", &[], &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_rejected_definition_leaves_the_registry_untouched() {
    let app = service();
    let first = definition("sp-3", "risk-band", "Patient.extension.valueCode", "active");
    request(&app, "POST", "/SearchParameter", &[], &first).await;
    let clash = br#"{"resourceType":"SearchParameter","id":"sp-4","name":"other","description":"a parameter","url":"urn:p:other","status":"active","code":"risk-band","base":["Patient"],"type":"token","expression":"Patient.extension.valueString"}"#;
    let reply = request(&app, "POST", "/SearchParameter", &[], clash).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    let duplicate = definition(
        "sp-3",
        "other-band",
        "Patient.extension.valueCode",
        "active",
    );
    let again = request(&app, "POST", "/SearchParameter", &[], &duplicate).await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    assert!(!diagnostics(&app, "/Patient?other-band=1")
        .await
        .contains("is supported"));
    let read = request(&app, "GET", "/SearchParameter/sp-4", &[], &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_definition_withdraws_the_parameter() {
    let app = service();
    let body = definition("sp-5", "risk-band", "Patient.extension.valueCode", "active");
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let reply = request(&app, "DELETE", "/SearchParameter/sp-5", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert!(!diagnostics(&app, "/Patient?risk-band=high")
        .await
        .contains("is supported"));
}

#[tokio::test]
async fn a_replaced_definition_replaces_the_registration() {
    let app = service();
    let body = definition("sp-6", "risk-band", "Patient.extension.valueCode", "active");
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let changed = definition(
        "sp-6",
        "risk-level",
        "Patient.extension.valueCode",
        "active",
    );
    let reply = request(&app, "PUT", "/SearchParameter/sp-6", &[], &changed).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(diagnostics(&app, "/Patient?risk-level=high")
        .await
        .contains("is supported"));
    assert!(!diagnostics(&app, "/Patient?risk-band=high")
        .await
        .contains("is supported"));
}

async fn statuses(app: &Service, method: &str, uri: &str, body: &[u8]) -> serde_json::Value {
    let reply = request(app, method, uri, &[], body).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri} gave {}", reply.body);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    let registered: Vec<serde_json::Value> = value["parameter"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["name"] != "unsupported")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({"resourceType": "Parameters", "parameter": registered})
}

fn status_of(value: &serde_json::Value, url: &str) -> Option<String> {
    value["parameter"].as_array()?.iter().find_map(|entry| {
        let parts = entry["part"].as_array()?;
        let found = |name: &str| {
            parts
                .iter()
                .find(|part| part["name"] == name)
                .and_then(|part| {
                    part["valueCode"]
                        .as_str()
                        .or_else(|| part["valueUri"].as_str())
                })
                .map(str::to_owned)
        };
        (found("url").as_deref() == Some(url)).then(|| found("status"))?
    })
}

#[tokio::test]
async fn the_status_endpoint_reports_index_readiness() {
    let app = service();
    let body = definition("sp-7", "risk-band", "Patient.extension.valueCode", "active");
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let listing = statuses(&app, "GET", "/SearchParameter/$status", &[]).await;
    assert_eq!(listing["resourceType"], "Parameters");
    assert_eq!(
        status_of(&listing, "urn:p:risk-band").as_deref(),
        Some("supported")
    );
    let one = statuses(
        &app,
        "GET",
        "/SearchParameter/$status?url=urn:p:risk-band",
        &[],
    )
    .await;
    assert_eq!(one["parameter"].as_array().map(Vec::len), Some(1));
    let missing = request(
        &app,
        "GET",
        "/SearchParameter/$status?url=urn:p:nonesuch",
        &[],
        &[],
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_status_endpoint_answers_a_posted_query() {
    let app = service();
    let body = definition("sp-8", "risk-band", "Patient.extension.valueCode", "active");
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let query = br#"{"resourceType":"Parameters","parameter":[{"name":"url","valueUri":"urn:p:risk-band"}]}"#;
    let listing = statuses(&app, "POST", "/SearchParameter/$status", query).await;
    assert_eq!(
        status_of(&listing, "urn:p:risk-band").as_deref(),
        Some("supported")
    );
}

#[tokio::test]
async fn the_status_of_one_named_parameter_is_answered_by_its_own_address() {
    let app = service();
    let body = definition(
        "sp-11",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let one = statuses(&app, "GET", "/SearchParameter/sp-11/$status", &[]).await;
    assert_eq!(one["resourceType"], "Parameters");
    assert_eq!(one["parameter"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        status_of(&one, "urn:p:risk-band").as_deref(),
        Some("supported")
    );
    let missing = request(&app, "GET", "/SearchParameter/nonesuch/$status", &[], &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let patient = request(
        &app,
        "POST",
        "/Patient",
        &[],
        br#"{"resourceType":"Patient"}"#,
    )
    .await;
    let held: serde_json::Value = serde_json::from_str(&patient.body).unwrap();
    let id = held["id"].as_str().unwrap().to_owned();
    let wrong = request(
        &app,
        "GET",
        &format!("/SearchParameter/{id}/$status"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        wrong.status,
        StatusCode::NOT_FOUND,
        "a resource of another type is not a search parameter: {}",
        wrong.body
    );
}

#[tokio::test]
async fn the_status_query_is_answered_from_a_form_too_long_for_an_address() {
    let app = service();
    let body = definition(
        "sp-12",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let whole = statuses(&app, "POST", "/SearchParameter/$status/_search", b"").await;
    assert_eq!(
        status_of(&whole, "urn:p:risk-band").as_deref(),
        Some("supported")
    );
    let one = statuses(
        &app,
        "POST",
        "/SearchParameter/$status/_search",
        b"url=urn:p:risk-band",
    )
    .await;
    assert_eq!(one["parameter"].as_array().map(Vec::len), Some(1));
    let missing = request(
        &app,
        "POST",
        "/SearchParameter/$status/_search",
        &[],
        b"url=urn:p:nonesuch",
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_parameter_is_disabled_and_enabled_through_the_status_endpoint() {
    let app = service();
    let body = definition("sp-9", "risk-band", "Patient.extension.valueCode", "active");
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let disabled = statuses(
        &app,
        "PUT",
        "/SearchParameter/$status?url=urn:p:risk-band&status=disabled",
        &[],
    )
    .await;
    assert_eq!(
        status_of(&disabled, "urn:p:risk-band").as_deref(),
        Some("disabled")
    );
    assert!(diagnostics(&app, "/Patient?risk-band=high")
        .await
        .contains("disabled"));
    let enabled = statuses(
        &app,
        "PUT",
        "/SearchParameter/$status?url=urn:p:risk-band&status=supported",
        &[],
    )
    .await;
    assert_eq!(
        status_of(&enabled, "urn:p:risk-band").as_deref(),
        Some("supported")
    );
    let refused = request(
        &app,
        "PUT",
        "/SearchParameter/$status?url=urn:p:risk-band&status=nonesuch",
        &[],
        &[],
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
}

fn banded(id: &str, code: &str) -> Vec<u8> {
    format!(
        r#"{{"resourceType":"Patient","id":"{id}","extension":[{{"url":"urn:x:band","valueCode":"{code}"}}]}}"#
    )
    .into_bytes()
}

fn part_of(entry: &serde_json::Value, name: &str) -> serde_json::Value {
    entry["part"]
        .as_array()
        .and_then(|parts| parts.iter().find(|part| part["name"] == name).cloned())
        .unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn a_reindex_backfills_and_makes_a_parameter_searchable() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &banded("pt-r1", "high")).await;
    request(&app, "POST", "/Patient", &[], &banded("pt-r2", "low")).await;
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        br#"{"resourceType":"Patient","id":"pt-r3"}"#,
    )
    .await;
    let body = definition(
        "sp-10",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    assert!(diagnostics(&app, "/Patient?risk-band=high")
        .await
        .contains("is supported"));
    let report = statuses(&app, "POST", "/SearchParameter/$reindex", &[]).await;
    let entry = &report["parameter"][0];
    assert_eq!(part_of(entry, "indexed")["valueInteger"], 2);
    assert_eq!(part_of(entry, "failures")["valueInteger"], 0);
    assert_eq!(
        found(&app, "/Patient?risk-band=high").await,
        vec!["pt-r1".to_owned()]
    );
    assert!(found(&app, "/Patient?risk-band=none").await.is_empty());
    let listing = statuses(&app, "GET", "/SearchParameter/$status", &[]).await;
    assert_eq!(
        status_of(&listing, "urn:p:risk-band").as_deref(),
        Some("searchable")
    );
}

#[tokio::test]
async fn a_reindex_reports_the_resources_it_could_not_index() {
    let app = service();
    request(
        &app,
        "POST",
        "/Patient",
        &[],
        &banded("pt-r4", "1980-04-01"),
    )
    .await;
    request(&app, "POST", "/Patient", &[], &banded("pt-r5", "whenever")).await;
    let body = br#"{"resourceType":"SearchParameter","id":"sp-11","name":"band-date","description":"a parameter","url":"urn:p:band-date","status":"active","code":"band-date","base":["Patient"],"type":"date","expression":"Patient.extension.valueCode"}"#;
    request(&app, "POST", "/SearchParameter", &[], body).await;
    let report = statuses(&app, "POST", "/SearchParameter/$reindex", &[]).await;
    let entry = &report["parameter"][0];
    assert_eq!(part_of(entry, "indexed")["valueInteger"], 1);
    assert_eq!(part_of(entry, "failures")["valueInteger"], 1);
    let failure = part_of(entry, "failure");
    assert_eq!(
        part_of(&failure, "resource")["valueString"],
        "Patient/pt-r5"
    );
    assert_eq!(
        found(&app, "/Patient?band-date=1980-04-01").await,
        vec!["pt-r4".to_owned()]
    );
}

#[tokio::test]
async fn a_retired_parameter_loses_its_index_on_the_next_reindex() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &banded("pt-r6", "high")).await;
    let body = definition(
        "sp-12",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    statuses(&app, "POST", "/SearchParameter/$reindex", &[]).await;
    let listing = statuses(
        &app,
        "PUT",
        "/SearchParameter/$status?url=urn:p:risk-band&status=disabled",
        &[],
    )
    .await;
    assert_eq!(
        status_of(&listing, "urn:p:risk-band").as_deref(),
        Some("pending-disable")
    );
    statuses(&app, "POST", "/SearchParameter/$reindex", &[]).await;
    let after = statuses(&app, "GET", "/SearchParameter/$status", &[]).await;
    assert_eq!(
        status_of(&after, "urn:p:risk-band").as_deref(),
        Some("disabled")
    );
}

fn instance(store: Arc<MemoryStore>) -> Service {
    let store: Arc<dyn ResourceStore> = store;
    Service::new(store, FhirVersion::R4, Vec::new())
}

async fn shared() -> Arc<MemoryStore> {
    Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })))
}

#[tokio::test]
async fn a_second_instance_converges_on_a_refresh() {
    let store = shared().await;
    let first = instance(Arc::clone(&store));
    let second = instance(Arc::clone(&store));
    request(&first, "POST", "/Patient", &[], &banded("pt-c1", "high")).await;
    let body = definition(
        "sp-13",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&first, "POST", "/SearchParameter", &[], &body).await;
    statuses(&first, "POST", "/SearchParameter/$reindex", &[]).await;
    assert_eq!(
        found(&first, "/Patient?risk-band=high").await,
        vec!["pt-c1".to_owned()]
    );
    assert!(!diagnostics(&second, "/Patient?risk-band=high")
        .await
        .contains("is supported"));
    statuses(&second, "POST", "/SearchParameter/$refresh", &[]).await;
    assert_eq!(
        found(&second, "/Patient?risk-band=high").await,
        vec!["pt-c1".to_owned()]
    );
    let again = statuses(&second, "POST", "/SearchParameter/$refresh", &[]).await;
    assert_eq!(
        status_of(&again, "urn:p:risk-band").as_deref(),
        Some("searchable")
    );
}

#[tokio::test]
async fn an_instance_started_over_a_loaded_store_answers_at_once() {
    let store = shared().await;
    let first = instance(Arc::clone(&store));
    request(&first, "POST", "/Patient", &[], &banded("pt-c2", "high")).await;
    let body = definition(
        "sp-14",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&first, "POST", "/SearchParameter", &[], &body).await;
    statuses(&first, "POST", "/SearchParameter/$reindex", &[]).await;
    let handle: Arc<dyn ResourceStore> = store.clone();
    let late = Service::started(handle, FhirVersion::R4, Vec::new())
        .await
        .unwrap();
    assert_eq!(
        found(&late, "/Patient?risk-band=high").await,
        vec!["pt-c2".to_owned()]
    );
}

#[tokio::test]
async fn a_withdrawn_definition_converges_too() {
    let store = shared().await;
    let first = instance(Arc::clone(&store));
    let second = instance(Arc::clone(&store));
    let body = definition(
        "sp-15",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&first, "POST", "/SearchParameter", &[], &body).await;
    statuses(&second, "POST", "/SearchParameter/$refresh", &[]).await;
    request(&first, "DELETE", "/SearchParameter/sp-15", &[], &[]).await;
    let listing = statuses(&second, "POST", "/SearchParameter/$refresh", &[]).await;
    assert!(
        listing["parameter"].as_array().is_none_or(Vec::is_empty),
        "{listing}"
    );
}

#[tokio::test]
async fn concurrent_definition_updates_never_lose_one() {
    let app = service();
    let body = definition(
        "sp-16",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let first = definition(
        "sp-16",
        "risk-alpha",
        "Patient.extension.valueCode",
        "active",
    );
    let second = definition(
        "sp-16",
        "risk-beta",
        "Patient.extension.valueCode",
        "active",
    );
    let etag = &[("if-match", "W/\"1\"")];
    let (left, right) = tokio::join!(
        request(&app, "PUT", "/SearchParameter/sp-16", etag, &first),
        request(&app, "PUT", "/SearchParameter/sp-16", etag, &second),
    );
    let outcomes = [left.status, right.status];
    assert!(outcomes.contains(&StatusCode::OK), "{outcomes:?}");
    assert!(
        outcomes.contains(&StatusCode::PRECONDITION_FAILED),
        "{outcomes:?}"
    );
    let (won, lost) = match left.status {
        StatusCode::OK => ("risk-alpha", "risk-beta"),
        _ => ("risk-beta", "risk-alpha"),
    };
    assert!(diagnostics(&app, &format!("/Patient?{won}=x"))
        .await
        .contains("is supported"));
    assert!(!diagnostics(&app, &format!("/Patient?{lost}=x"))
        .await
        .contains("is supported"));
    let listing = statuses(&app, "GET", "/SearchParameter/$status", &[]).await;
    assert_eq!(listing["parameter"].as_array().map(Vec::len), Some(1));
    let stored = request(&app, "GET", "/SearchParameter/sp-16", &[], &[]).await;
    assert!(stored.body.contains(won), "{}", stored.body);
}

#[tokio::test]
async fn a_stale_definition_update_changes_nothing() {
    let app = service();
    let body = definition(
        "sp-17",
        "risk-band",
        "Patient.extension.valueCode",
        "active",
    );
    request(&app, "POST", "/SearchParameter", &[], &body).await;
    let next = definition(
        "sp-17",
        "risk-band",
        "Patient.extension.valueString",
        "active",
    );
    request(&app, "PUT", "/SearchParameter/sp-17", &[], &next).await;
    let stale = definition(
        "sp-17",
        "risk-stale",
        "Patient.extension.valueCode",
        "active",
    );
    let reply = request(
        &app,
        "PUT",
        "/SearchParameter/sp-17",
        &[("if-match", "W/\"1\"")],
        &stale,
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        reply.body
    );
    assert!(!diagnostics(&app, "/Patient?risk-stale=x")
        .await
        .contains("is supported"));
    assert!(diagnostics(&app, "/Patient?risk-band=x")
        .await
        .contains("is supported"));
}

#[tokio::test]
async fn a_body_with_an_element_the_type_does_not_define_is_refused() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-9","favourite":"tea"}"#.to_vec();
    let reply = request(&app, "POST", "/Patient", &[], &body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("structure"), "{}", reply.body);
    assert!(reply.body.contains("favourite"), "{}", reply.body);
    let read = request(&app, "GET", "/Patient/pt-9", &[], &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_body_with_a_primitive_of_the_wrong_shape_is_refused() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-8","active":"yes"}"#.to_vec();
    let reply = request(&app, "POST", "/Patient", &[], &body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("active"), "{}", reply.body);
}

#[tokio::test]
async fn a_body_with_a_code_outside_a_bound_value_set_is_refused() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-7","gender":"lady"}"#.to_vec();
    let reply = request(&app, "POST", "/Patient", &[], &body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("binding"), "{}", reply.body);
}

#[tokio::test]
async fn an_update_with_a_body_that_does_not_match_its_type_leaves_the_stored_one() {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("pt-6", true)).await;
    let body = br#"{"resourceType":"Patient","id":"pt-6","favourite":"tea"}"#.to_vec();
    let reply = request(
        &app,
        "PUT",
        "/Patient/pt-6",
        &[("if-match", "W/\"1\"")],
        &body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let read = request(&app, "GET", "/Patient/pt-6", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(header(&read, "etag"), "W/\"1\"");
}

#[tokio::test]
async fn a_store_that_cannot_answer_is_a_503_with_a_retry_hint() {
    let app = Service::new(
        Arc::new(FailingStore(|| {
            Error::Unavailable("the store is busy".to_owned())
        })),
        FhirVersion::R4,
        vec![],
    );
    let reply = request(&app, "GET", "/Patient/waiting", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header(&reply, "retry-after"), "2");
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["issue"][0]["code"], "transient");
    let detail = value["issue"][0]["diagnostics"].as_str().unwrap();
    assert!(detail.contains("may be repeated"), "{detail}");
}

#[tokio::test]
async fn a_refusal_names_the_published_code_for_what_went_wrong() {
    let app = service();
    let missing = request(&app, "GET", "/Patient/nobody", &[], &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let value: serde_json::Value = serde_json::from_str(&missing.body).unwrap();
    assert_eq!(
        value["issue"][0]["details"]["coding"][0]["system"],
        fhir_core::OUTCOME_SYSTEM
    );
    assert_eq!(
        value["issue"][0]["details"]["coding"][0]["code"],
        "MSG_NO_EXIST"
    );

    let unknown = request(&app, "GET", "/Patient?_nonesuch=1", &[], &[]).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&unknown.body).unwrap();
    assert_eq!(
        value["issue"][0]["details"]["coding"][0]["code"],
        "MSG_PARAM_UNKNOWN"
    );
}

#[tokio::test]
async fn a_page_asked_for_by_offset_is_told_where_the_next_page_comes_from() {
    let app = service();
    let reply = request(&app, "GET", "/Patient?_offset=20", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(
        value["issue"][0]["details"]["coding"][0]["code"],
        "MSG_PARAM_UNKNOWN"
    );
    let told = value["issue"][0]["diagnostics"].as_str().unwrap();
    assert!(told.contains("next link"), "{told}");
    assert!(told.contains("ct"), "{told}");
}
