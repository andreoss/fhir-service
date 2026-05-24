use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Counting, Limits, Paging, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service(paging: Paging, limits: Limits) -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .paging(paging)
        .bounded_by(limits)
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

async fn holding(router: &axum::Router, how_many: usize) {
    for index in 0..how_many {
        let (status, body) = ask(
            router,
            "PUT",
            &format!("/Patient/p{index}"),
            Some(json!({
                "resourceType": "Patient",
                "id": format!("p{index}"),
                "name": [{"family": format!("Family{index:03}")}],
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
}

async fn bundle_of(router: &axum::Router, uri: &str) -> Value {
    let (status, body) = ask(router, "GET", uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).expect("a bundle")
}

fn entries(bundle: &Value) -> usize {
    bundle["entry"].as_array().map(Vec::len).unwrap_or_default()
}

#[tokio::test]
async fn a_page_holds_what_the_operator_said() {
    let router = service(Paging::new(3, 10).expect("a setting"), Limits::unbounded());
    holding(&router, 6).await;
    let held = bundle_of(&router, "/Patient").await;
    assert_eq!(
        entries(&held),
        3,
        "and not the twenty this build used to serve"
    );
}

#[tokio::test]
async fn a_client_may_ask_for_more_but_not_past_the_bound() {
    let router = service(Paging::new(3, 5).expect("a setting"), Limits::unbounded());
    holding(&router, 8).await;
    assert_eq!(entries(&bundle_of(&router, "/Patient?_count=4").await), 4);
    assert_eq!(
        entries(&bundle_of(&router, "/Patient?_count=500").await),
        5,
        "the bound is what a bound is"
    );
}

#[tokio::test]
async fn an_operator_may_stop_counting_what_nobody_asked_for() {
    let counting = service(Paging::new(2, 10).expect("a setting"), Limits::unbounded());
    holding(&counting, 4).await;
    assert_eq!(
        bundle_of(&counting, "/Patient").await["total"],
        4,
        "which is what this build has always answered"
    );

    let quiet = service(
        Paging::new(2, 10)
            .expect("a setting")
            .counting(Counting::None),
        Limits::unbounded(),
    );
    holding(&quiet, 4).await;
    let held = bundle_of(&quiet, "/Patient").await;
    assert!(
        held["total"].is_null(),
        "counting the whole match is a second query, and an operator may \
         decline to run it for a client that did not ask: {held}"
    );
    let asked = bundle_of(&quiet, "/Patient?_total=accurate").await;
    assert_eq!(asked["total"], 4, "a client that asks still gets one");
}

#[tokio::test]
async fn a_default_sort_orders_what_the_client_did_not_order() {
    let router = service(
        Paging::new(10, 10)
            .expect("a setting")
            .sorting_by(Some("-family".to_owned())),
        Limits::unbounded(),
    );
    holding(&router, 3).await;
    let held = bundle_of(&router, "/Patient").await;
    let families: Vec<&str> = held["entry"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|entry| entry["resource"]["name"][0]["family"].as_str())
        .collect();
    assert_eq!(families, vec!["Family002", "Family001", "Family000"]);

    let asked = bundle_of(&router, "/Patient?_sort=family").await;
    let ascending: Vec<&str> = asked["entry"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|entry| entry["resource"]["name"][0]["family"].as_str())
        .collect();
    assert_eq!(
        ascending,
        vec!["Family000", "Family001", "Family002"],
        "a client that named a sort gets the one it named"
    );
}

#[tokio::test]
async fn a_count_summary_still_counts_under_a_default_of_none() {
    let router = service(Paging::new(2, 10).expect("a setting"), Limits::unbounded());
    holding(&router, 5).await;
    let held = bundle_of(&router, "/Patient?_summary=count").await;
    assert_eq!(held["total"], 5);
    assert_eq!(
        entries(&held),
        0,
        "a count is a count and carries no entries"
    );
}

#[tokio::test]
async fn a_body_larger_than_the_bound_is_refused() {
    let router = service(
        Paging::default(),
        Limits::new(Some(400), None).expect("a bound"),
    );
    let large = json!({
        "resourceType": "Patient",
        "id": "p1",
        "name": [{"family": "x".repeat(1_000)}],
    });
    let (status, body) = ask(&router, "PUT", "/Patient/p1", Some(large)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert!(body.contains("400"), "the refusal says the bound: {body}");
}

#[tokio::test]
async fn a_body_within_the_bound_is_written() {
    let router = service(
        Paging::default(),
        Limits::new(Some(400), None).expect("a bound"),
    );
    let (status, body) = ask(
        &router,
        "PUT",
        "/Patient/p1",
        Some(json!({"resourceType": "Patient", "id": "p1", "active": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn a_bundle_carrying_more_entries_than_the_bound_is_refused() {
    let router = service(
        Paging::default(),
        Limits::new(None, Some(2)).expect("a bound"),
    );
    let entry = |index: usize| {
        json!({
            "resource": {"resourceType": "Patient", "id": format!("p{index}"), "active": true},
            "request": {"method": "PUT", "url": format!("Patient/p{index}")},
        })
    };
    let (status, body) = ask(
        &router,
        "POST",
        "/",
        Some(json!({
            "resourceType": "Bundle",
            "type": "transaction",
            "entry": [entry(1), entry(2), entry(3)],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("3"), "the refusal says what was sent: {body}");

    let (within, body) = ask(
        &router,
        "POST",
        "/",
        Some(json!({
            "resourceType": "Bundle",
            "type": "transaction",
            "entry": [entry(1), entry(2)],
        })),
    )
    .await;
    assert_eq!(within, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn an_unbounded_instance_is_what_this_build_always_was() {
    let router = service(Paging::default(), Limits::unbounded());
    holding(&router, 25).await;
    assert_eq!(
        entries(&bundle_of(&router, "/Patient").await),
        20,
        "twenty by default, as it always was"
    );
    assert_eq!(
        entries(&bundle_of(&router, "/Patient?_count=5000").await),
        25,
        "capped at a hundred, and there are twenty-five"
    );
}

fn padded(bytes: usize) -> Value {
    json!({
        "resourceType": "Basic",
        "id": "padded",
        "code": {"text": "x".repeat(bytes)},
    })
}

#[tokio::test]
async fn a_body_nothing_bounds_is_taken_however_large_it_is() {
    let router = service(Paging::default(), Limits::unbounded());
    let (status, body) = ask(
        &router,
        "PUT",
        "/Basic/padded",
        Some(padded(3 * 1024 * 1024)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        body.chars().take(300).collect::<String>()
    );
}

#[tokio::test]
async fn a_body_past_the_bound_is_refused_in_an_outcome() {
    let router = service(
        Paging::default(),
        Limits::new(Some(1_000_000), None).expect("a bound"),
    );
    let (status, body) = ask(
        &router,
        "PUT",
        "/Basic/padded",
        Some(padded(3 * 1024 * 1024)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        body.chars().take(300).collect::<String>()
    );
    let held: Value = serde_json::from_str(&body).expect("an outcome in json");
    assert_eq!(held["resourceType"], "OperationOutcome");
    assert_eq!(held["issue"][0]["code"], "too-costly");
    assert!(
        held["issue"][0]["diagnostics"]
            .as_str()
            .is_some_and(|said| said.contains("1000000")),
        "{body}"
    );
}
