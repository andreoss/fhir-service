use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Administration, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

fn one_door() -> axum::Router {
    service().router()
}

fn two_doors() -> axum::Router {
    service()
        .with_administration(
            Administration::restricted_to(["127.0.0.0/8", "10.1.0.0/16"])
                .expect("two networks are a restriction"),
        )
        .router()
}

async fn from(
    router: &axum::Router,
    address: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let held = match &body {
        Some(body) => Body::from(serde_json::to_vec(body).expect("a body serialises")),
        None => Body::empty(),
    };
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .body(held)
        .expect("a request is built");
    if let Some(address) = address {
        let address: SocketAddr = address.parse().expect("an address parses");
        request.extensions_mut().insert(ConnectInfo(address));
    }
    let response = router
        .clone()
        .oneshot(request)
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

fn parameter() -> Value {
    json!({
        "resourceType": "SearchParameter",
        "url": "http://example.org/SearchParameter/colour",
        "name": "colour",
        "status": "active",
        "description": "a colour",
        "code": "colour",
        "base": ["Patient"],
        "type": "string",
        "expression": "Patient.name"
    })
}

#[tokio::test]
async fn one_door_serves_everything_as_before() {
    let router = one_door();
    let (status, _) = from(&router, None, "GET", "/SearchParameter", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an instance configuring nothing keeps one door"
    );
}

#[tokio::test]
async fn the_administration_path_is_nothing_while_the_door_is_shut() {
    let router = one_door();
    let (status, body) = from(
        &router,
        None,
        "GET",
        "/administration/SearchParameter",
        None,
    )
    .await;
    assert!(
        status.is_client_error(),
        "with one door the prefix is not a prefix, only a type that is no type: {status} {body}"
    );
    assert!(
        body.contains("administration") && !body.contains("not served on this door"),
        "and the refusal is the ordinary one: {body}"
    );
}

#[tokio::test]
async fn an_admitted_network_reaches_the_other_door() {
    let router = two_doors();
    let (status, body) = from(
        &router,
        Some("127.0.0.1:9000"),
        "GET",
        "/administration/SearchParameter",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a bundle is json");
    assert_eq!(held["resourceType"], "Bundle");
}

#[tokio::test]
async fn a_network_outside_the_list_is_refused() {
    let router = two_doors();
    let (status, body) = from(
        &router,
        Some("192.168.4.4:9000"),
        "GET",
        "/administration/SearchParameter",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("192.168.4.4"),
        "the refusal names what was refused: {body}"
    );
}

#[tokio::test]
async fn a_request_carrying_no_address_is_refused() {
    let router = two_doors();
    let (status, body) = from(
        &router,
        None,
        "GET",
        "/administration/SearchParameter",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an unjudged request is not admitted: {body}"
    );
}

#[tokio::test]
async fn the_same_path_is_refused_on_the_clinical_door() {
    let router = two_doors();
    let (status, body) = from(
        &router,
        Some("127.0.0.1:9000"),
        "GET",
        "/SearchParameter",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the clinical door does not serve it even from an admitted address: {body}"
    );
}

#[tokio::test]
async fn a_clinical_type_is_still_served_on_the_clinical_door() {
    let router = two_doors();
    let (status, body) = from(&router, Some("8.8.8.8:9000"), "GET", "/Patient", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_write_through_the_other_door_is_a_write() {
    let router = two_doors();
    let (status, body) = from(
        &router,
        Some("10.1.2.3:9000"),
        "POST",
        "/administration/SearchParameter",
        Some(parameter()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, read) = from(
        &router,
        Some("10.1.2.3:9000"),
        "GET",
        "/administration/SearchParameter",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert!(
        read.contains("http://example.org/SearchParameter/colour"),
        "what was written through the door is read through it: {read}"
    );
}

#[tokio::test]
async fn a_maintenance_operation_moves_with_the_types() {
    let router = two_doors();
    let (clinical, _) = from(&router, Some("127.0.0.1:9000"), "POST", "/$reindex", None).await;
    assert_eq!(
        clinical,
        StatusCode::NOT_FOUND,
        "a reindex is not a clinical request"
    );
    let (refused, _) = from(
        &router,
        Some("192.168.1.1:9000"),
        "POST",
        "/administration/$reindex",
        None,
    )
    .await;
    assert_eq!(refused, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_terminology_operation_stays_clinical() {
    let router = two_doors();
    let (status, body) = from(
        &router,
        Some("8.8.8.8:9000"),
        "GET",
        "/ValueSet/$expand?url=http://hl7.org/fhir/ValueSet/administrative-gender",
        None,
    )
    .await;
    assert!(
        !body.contains("not served on this door"),
        "asking a terminology question is not administering the server: {status} {body}"
    );
}

#[tokio::test]
async fn a_query_survives_the_door() {
    let router = two_doors();
    let (status, body) = from(
        &router,
        Some("127.0.0.1:9000"),
        "GET",
        "/administration/SearchParameter?name=colour",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a bundle is json");
    assert_eq!(held["resourceType"], "Bundle");
    let link = held["link"][0]["url"].as_str().unwrap_or_default();
    assert!(
        link.contains("name=colour"),
        "the query the client sent is the query that was run: {link}"
    );
}

#[test]
fn a_door_admitting_nobody_is_refused_at_startup() {
    let empty: [&str; 0] = [];
    assert!(Administration::restricted_to(empty).is_err());
}

#[test]
fn what_is_not_a_network_is_refused_at_startup() {
    assert!(Administration::restricted_to(["10.1.0.0/48"]).is_err());
    assert!(Administration::restricted_to(["not-a-network"]).is_err());
}
