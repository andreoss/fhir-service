


use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Resettable, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service(reset: Resettable) -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .resettable(reset)
        .router()
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

fn confirming(name: &str) -> Value {
    json!({
        "resourceType": "Parameters",
        "parameter": [{"name": "confirm", "valueString": name}],
    })
}

async fn holding(router: &axum::Router) {
    let (status, body) = ask(
        router,
        "POST",
        "/Patient",
        Some(json!({"resourceType": "Patient", "active": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn count(router: &axum::Router) -> u64 {
    let (status, body) = ask(router, "GET", "/Patient?_summary=count", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a bundle");
    held["total"].as_u64().unwrap_or_default()
}

#[tokio::test]
async fn an_instance_that_was_not_told_it_may_be_emptied_refuses() {
    let router = service(Resettable::never());
    holding(&router).await;
    let (status, body) = ask(&router, "POST", "/$reset", Some(confirming("anything"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("deliberately"), "{body}");
    assert_eq!(count(&router).await, 1, "and it still holds what it held");
}

#[tokio::test]
async fn a_reset_naming_nothing_is_refused() {
    let router = service(Resettable::named("staging").expect("a name"));
    holding(&router).await;
    let (status, body) = ask(&router, "POST", "/$reset", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("staging"),
        "the refusal says what must be named: {body}"
    );
    assert_eq!(count(&router).await, 1);
}

#[tokio::test]
async fn a_reset_naming_another_instance_is_refused() {
    let router = service(Resettable::named("staging").expect("a name"));
    holding(&router).await;
    let (status, body) = ask(&router, "POST", "/$reset", Some(confirming("production"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        count(&router).await,
        1,
        "a request sent to the wrong address is the way this goes wrong"
    );
}

#[tokio::test]
async fn a_reset_naming_this_instance_empties_it() {
    let router = service(Resettable::named("staging").expect("a name"));
    holding(&router).await;
    holding(&router).await;
    assert_eq!(count(&router).await, 2);
    let (status, body) = ask(&router, "POST", "/$reset", Some(confirming("staging"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("2 resources removed"), "{body}");
    assert_eq!(count(&router).await, 0);
}

#[tokio::test]
async fn an_instance_emptied_is_an_instance_that_still_works() {
    let router = service(Resettable::named("staging").expect("a name"));
    holding(&router).await;
    ask(&router, "POST", "/$reset", Some(confirming("staging"))).await;
    holding(&router).await;
    assert_eq!(
        count(&router).await,
        1,
        "what was emptied can be written to again"
    );
}
