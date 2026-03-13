use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Restricted, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service(restricted: Restricted) -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .serving(restricted)
        .router()
}

fn narrowed() -> axum::Router {
    service(Restricted::parse(["Patient", "Observation"], ["family", "code"]).expect("known names"))
}

async fn ask(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let held = match &body {
        Some(body) => Body::from(serde_json::to_vec(body).expect("a body")),
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "localhost")
                .header("content-type", "application/fhir+json")
                .body(held)
                .expect("a request"),
        )
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

#[tokio::test]
async fn a_type_the_instance_serves_is_served() {
    let router = narrowed();
    let (status, body) = ask(
        &router,
        "PUT",
        "/Patient/p1",
        Some(json!({"resourceType": "Patient", "id": "p1", "active": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        ask(&router, "GET", "/Patient/p1", None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_type_it_does_not_serve_is_refused_with_a_reason() {
    let router = narrowed();
    let (status, body) = ask(&router, "GET", "/Medication", None).await;
    assert!(status.is_client_error(), "{status} {body}");
    assert!(
        body.contains("not among the types this instance serves"),
        "a client that asked anyway is owed a reason: {body}"
    );
    let (written, _) = ask(
        &router,
        "PUT",
        "/Medication/m1",
        Some(json!({"resourceType": "Medication", "id": "m1"})),
    )
    .await;
    assert!(written.is_client_error(), "and cannot be written either");
}

#[tokio::test]
async fn the_statement_does_not_advertise_what_the_instance_refuses() {
    let router = narrowed();
    let (status, body) = ask(&router, "GET", "/metadata", None).await;
    assert_eq!(status, StatusCode::OK);
    let held: Value = serde_json::from_str(&body).expect("a statement");
    let types: Vec<&str> = held["rest"][0]["resource"]
        .as_array()
        .expect("resources")
        .iter()
        .filter_map(|entry| entry["type"].as_str())
        .collect();
    assert_eq!(types, vec!["Observation", "Patient"]);
}

#[tokio::test]
async fn the_statement_does_not_advertise_a_parameter_it_will_not_answer() {
    let router = narrowed();
    let (_, body) = ask(&router, "GET", "/metadata", None).await;
    let held: Value = serde_json::from_str(&body).expect("a statement");
    let patient = held["rest"][0]["resource"]
        .as_array()
        .expect("resources")
        .iter()
        .find(|entry| entry["type"] == "Patient")
        .expect("Patient is served");
    let names: Vec<&str> = patient["searchParam"]
        .as_array()
        .map(|held| {
            held.iter()
                .filter_map(|entry| entry["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(names.contains(&"family"));
    assert!(
        !names.contains(&"gender"),
        "a client reads the statement to decide what to send: {names:?}"
    );
}

#[tokio::test]
async fn a_search_by_a_parameter_it_answers_is_answered() {
    let router = narrowed();
    ask(
        &router,
        "PUT",
        "/Patient/p1",
        Some(json!({"resourceType": "Patient", "id": "p1", "name": [{"family": "Stone"}]})),
    )
    .await;
    let (status, body) = ask(&router, "GET", "/Patient?family=Stone", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a bundle");
    assert_eq!(held["entry"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn a_search_by_one_it_does_not_answer_is_refused() {
    let router = narrowed();
    let (status, body) = ask(&router, "GET", "/Patient?gender=female", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("not among the search parameters this instance answers"),
        "{body}"
    );
}

#[tokio::test]
async fn paging_a_narrowed_instance_still_works() {
    let router = narrowed();
    for index in 0..3 {
        ask(
            &router,
            "PUT",
            &format!("/Patient/p{index}"),
            Some(json!({"resourceType": "Patient", "id": format!("p{index}"), "active": true})),
        )
        .await;
    }
    let (status, body) = ask(&router, "GET", "/Patient?_count=2&_sort=_id", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the result-control parameters are how a client reads a page: {body}"
    );
}

#[tokio::test]
async fn an_instance_naming_nothing_serves_everything() {
    let router = service(Restricted::everything());
    let (status, _) = ask(&router, "GET", "/Medication", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ask(&router, "GET", "/Patient?gender=female", None).await.0,
        StatusCode::OK,
        "which is what this build did before the setting existed"
    );
}
