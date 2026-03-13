use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{MediaType, Service, Unchanged};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service(unchanged: Unchanged) -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .skipping_unchanged(unchanged)
        .router()
}

async fn ask(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    prefer: Option<&str>,
) -> (StatusCode, String) {
    let held = match &body {
        Some(body) => Body::from(serde_json::to_vec(body).expect("a body")),
        None => Body::empty(),
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json");
    if let Some(prefer) = prefer {
        builder = builder.header("prefer", prefer);
    }
    let response = router
        .clone()
        .oneshot(builder.body(held).expect("a request"))
        .await
        .expect("an answer");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn patient(active: bool) -> Value {
    json!({"resourceType": "Patient", "id": "p1", "active": active})
}

async fn version_of(router: &axum::Router) -> String {
    let (status, body) = ask(router, "GET", "/Patient/p1", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a resource");
    held["meta"]["versionId"]
        .as_str()
        .expect("a version")
        .to_owned()
}

#[tokio::test]
async fn a_resubmitted_resource_writes_no_version() {
    let router = service(Unchanged::parse("").expect("a setting"));
    let (created, _) = ask(&router, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    assert_eq!(created, StatusCode::CREATED);
    assert_eq!(version_of(&router).await, "1");

    let (again, body) = ask(&router, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    assert_eq!(again, StatusCode::OK, "{body}");
    assert_eq!(
        version_of(&router).await,
        "1",
        "the history of a record that did not change did not grow"
    );
}

#[tokio::test]
async fn a_changed_resource_still_writes() {
    let router = service(Unchanged::parse("").expect("a setting"));
    ask(&router, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    let (status, _) = ask(&router, "PUT", "/Patient/p1", Some(patient(false)), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(version_of(&router).await, "2");
}

#[tokio::test]
async fn the_store_skipped_the_write_before_this_setting_existed() {
    let router = service(Unchanged::silent());
    ask(&router, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    let (status, body) = ask(
        &router,
        "PUT",
        "/Patient/p1",
        Some(patient(true)),
        Some("return=OperationOutcome"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        version_of(&router).await,
        "1",
        "the adapters have always compared content and refused an identical write"
    );
    assert!(
        !body.contains("no changes were performed"),
        "what the setting adds is saying so, and it was not asked: {body}"
    );
}

#[tokio::test]
async fn a_client_that_asked_for_an_outcome_is_told_nothing_happened() {
    let router = service(Unchanged::parse("").expect("a setting"));
    ask(&router, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    let (status, body) = ask(
        &router,
        "PUT",
        "/Patient/p1",
        Some(patient(true)),
        Some("return=OperationOutcome"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("no changes were performed"),
        "and it says so plainly: {body}"
    );
}

#[tokio::test]
async fn a_patch_that_changes_nothing_writes_nothing() {
    let router = service(Unchanged::parse("").expect("a setting"));
    ask(&router, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    let patch = json!([{"op": "replace", "path": "/active", "value": true}]);
    let (status, body) = ask(&router, "PATCH", "/Patient/p1", Some(patch), None).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(version_of(&router).await, "1");
}

#[tokio::test]
async fn a_label_counts_as_a_change_unless_it_is_ignored() {
    let counting = service(Unchanged::parse("").expect("a setting"));
    ask(&counting, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    let mut labelled = patient(true);
    labelled["meta"] = json!({"tag": [{"system": "urn:t", "code": "one"}]});
    ask(
        &counting,
        "PUT",
        "/Patient/p1",
        Some(labelled.clone()),
        None,
    )
    .await;
    assert_eq!(
        version_of(&counting).await,
        "2",
        "a resource that gained a tag changed"
    );

    let ignoring = service(Unchanged::parse("tag").expect("a setting"));
    ask(&ignoring, "PUT", "/Patient/p1", Some(patient(true)), None).await;
    ask(&ignoring, "PUT", "/Patient/p1", Some(labelled), None).await;
    assert_eq!(
        version_of(&ignoring).await,
        "1",
        "unless the operator said tags do not count"
    );
}

#[tokio::test]
async fn a_request_naming_no_media_type_is_answered_in_the_configured_one() {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let router = Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .answering(MediaType::FhirXml)
        .router();
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metadata")
                .header("host", "localhost")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|held| held.to_str().ok()),
        Some("application/fhir+xml"),
        "a client that names nothing is not asking for JSON, it is not minding"
    );
}

#[tokio::test]
async fn a_request_that_named_a_media_type_still_gets_that_one() {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let router = Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .answering(MediaType::FhirXml)
        .router();
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metadata")
                .header("host", "localhost")
                .header("accept", "application/fhir+json")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("an answer");
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|held| held.to_str().ok()),
        Some("application/fhir+json"),
        "the setting is a fallback, not an override"
    );
}
