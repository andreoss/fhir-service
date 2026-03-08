


use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Service, Tenancy};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const SYSTEM: &str = "urn:example:tenant";

fn tenanted() -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .with_tenancy(Tenancy::by_label(SYSTEM, "tenant").expect("a system and a claim"))
        .router()
}

fn untenanted() -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).router()
}

async fn ask(
    router: &axum::Router,
    tenant: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    asking(router, tenant, method, uri, body, false).await
}

async fn asking(
    router: &axum::Router,
    tenant: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<Value>,
    reveal: bool,
) -> (StatusCode, String) {
    let held = match &body {
        Some(body) => Body::from(serde_json::to_vec(body).expect("a body serialises")),
        None => Body::empty(),
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json");
    if let Some(tenant) = tenant {
        builder = builder.header("x-tenant", tenant);
    }
    if reveal {
        builder = builder.header("x-tenant-label", "show");
    }
    let response = router
        .clone()
        .oneshot(builder.body(held).expect("a request is built"))
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body is read")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn patient(name: &str) -> Value {
    json!({
        "resourceType": "Patient",
        "name": [{"family": name}]
    })
}

fn id_of(body: &str) -> String {
    let held: Value = serde_json::from_str(body).expect("a resource is json");
    held["id"]
        .as_str()
        .expect("a written resource has an id")
        .to_owned()
}

#[tokio::test]
async fn a_resource_one_tenant_wrote_is_not_read_by_another() {
    let router = tenanted();
    let (status, body) = ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = id_of(&body);

    let (mine, _) = ask(&router, Some("one"), "GET", &format!("/Patient/{id}"), None).await;
    assert_eq!(mine, StatusCode::OK, "a tenant reads its own");

    let (theirs, body) = ask(&router, Some("two"), "GET", &format!("/Patient/{id}"), None).await;
    assert_eq!(
        theirs,
        StatusCode::NOT_FOUND,
        "and another tenant is told it is not there, not that it may not see it: {body}"
    );
}

#[tokio::test]
async fn a_search_answers_only_the_asking_tenant() {
    let router = tenanted();
    ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    ask(
        &router,
        Some("two"),
        "POST",
        "/Patient",
        Some(patient("Two")),
    )
    .await;

    let (status, body) = ask(&router, Some("one"), "GET", "/Patient", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("One"), "its own is there: {body}");
    assert!(!body.contains("Two"), "and the other's is not: {body}");

    let (_, body) = ask(&router, Some("two"), "GET", "/Patient", None).await;
    assert!(body.contains("Two") && !body.contains("One"), "{body}");
}

#[tokio::test]
async fn the_label_is_not_in_what_a_tenant_reads() {
    let router = tenanted();
    let (_, body) = ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    assert!(
        !body.contains(SYSTEM),
        "the answer to the write carries no label: {body}"
    );
    let id = id_of(&body);
    let (_, read) = ask(&router, Some("one"), "GET", &format!("/Patient/{id}"), None).await;
    assert!(!read.contains(SYSTEM), "nor does the read: {read}");
    let (_, searched) = ask(&router, Some("one"), "GET", "/Patient", None).await;
    assert!(!searched.contains(SYSTEM), "nor the bundle: {searched}");
}

#[tokio::test]
async fn an_operator_may_ask_to_see_the_label() {
    let router = tenanted();
    let (_, body) = ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    let id = id_of(&body);
    let (status, shown) = asking(
        &router,
        Some("one"),
        "GET",
        &format!("/Patient/{id}"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        shown.contains(SYSTEM) && shown.contains("\"one\""),
        "the label is there when asked for: {shown}"
    );
}

#[tokio::test]
async fn a_label_the_client_wrote_itself_does_not_count() {
    let router = tenanted();
    let mut forged = patient("One");
    forged["meta"] = json!({"security": [{"system": SYSTEM, "code": "two"}]});
    let (status, body) = ask(&router, Some("one"), "POST", "/Patient", Some(forged)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = id_of(&body);
    let (theirs, _) = ask(&router, Some("two"), "GET", &format!("/Patient/{id}"), None).await;
    assert_eq!(
        theirs,
        StatusCode::NOT_FOUND,
        "the tenant a resource belongs to is the one that wrote it, not the one \
         the body claims"
    );
    let (mine, _) = ask(&router, Some("one"), "GET", &format!("/Patient/{id}"), None).await;
    assert_eq!(mine, StatusCode::OK);
}

#[tokio::test]
async fn a_patch_cannot_move_a_resource_between_tenants() {
    let router = tenanted();
    let (_, body) = ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    let id = id_of(&body);
    let patch = json!([{
        "op": "add",
        "path": "/meta",
        "value": {"security": [{"system": SYSTEM, "code": "two"}]}
    }]);
    let (status, said) = ask(
        &router,
        Some("one"),
        "PATCH",
        &format!("/Patient/{id}"),
        Some(patch),
    )
    .await;
    assert!(status.is_success(), "{said}");
    let (theirs, _) = ask(&router, Some("two"), "GET", &format!("/Patient/{id}"), None).await;
    assert_eq!(theirs, StatusCode::NOT_FOUND, "it did not move");
    let (mine, _) = ask(&router, Some("one"), "GET", &format!("/Patient/{id}"), None).await;
    assert_eq!(mine, StatusCode::OK, "and it is still where it was");
}

#[tokio::test]
async fn an_update_by_another_tenant_does_not_overwrite() {
    let router = tenanted();
    let (_, body) = ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    let id = id_of(&body);
    let (status, said) = ask(
        &router,
        Some("two"),
        "PUT",
        &format!("/Patient/{id}"),
        Some(patient("Two")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a write onto another tenant's resource is refused as though it were \
         not there, which to this tenant it is not: {said}"
    );
    let (_, mine) = ask(&router, Some("one"), "GET", &format!("/Patient/{id}"), None).await;
    assert!(
        mine.contains("One") && !mine.contains("Two"),
        "what the first tenant wrote is what it still reads: {mine}"
    );
}

#[tokio::test]
async fn a_request_naming_no_tenant_is_refused() {
    let router = tenanted();
    let (status, body) = ask(&router, None, "GET", "/Patient", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("names none"), "{body}");
}

#[tokio::test]
async fn history_is_refused_and_says_why() {
    let router = tenanted();
    let (_, written) = ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    let id = id_of(&written);
    let instance = format!("/Patient/{id}/_history");
    for path in ["/_history", "/Patient/_history", instance.as_str()] {
        let (status, body) = ask(&router, Some("one"), "GET", path, None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{path}: {body}");
        assert!(
            body.contains("several tenants"),
            "{path}: the refusal says why: {body}"
        );
    }
}

#[tokio::test]
async fn reading_a_past_version_is_refused() {
    let router = tenanted();
    let (_, body) = ask(
        &router,
        Some("one"),
        "POST",
        "/Patient",
        Some(patient("One")),
    )
    .await;
    let id = id_of(&body);
    let (status, said) = ask(
        &router,
        Some("one"),
        "GET",
        &format!("/Patient/{id}/_history/1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{said}");
}

#[tokio::test]
async fn an_instance_with_one_tenant_is_unchanged() {
    let router = untenanted();
    let (status, body) = ask(&router, None, "POST", "/Patient", Some(patient("One"))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = id_of(&body);
    let (read, _) = ask(&router, None, "GET", &format!("/Patient/{id}"), None).await;
    assert_eq!(read, StatusCode::OK);
    let (history, _) = ask(&router, None, "GET", "/_history", None).await;
    assert_eq!(history, StatusCode::OK, "history is served as it was");
}

#[test]
fn a_tenancy_without_a_system_or_a_claim_is_refused() {
    assert!(Tenancy::by_label("", "tenant").is_err());
    assert!(Tenancy::by_label(SYSTEM, " ").is_err());
}
