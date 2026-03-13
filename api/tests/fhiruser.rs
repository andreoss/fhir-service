use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn router() -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).router()
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

fn looking(parameters: Vec<Value>) -> Value {
    json!({"resourceType": "Parameters", "parameter": parameters})
}

fn named(name: &str, value: &str) -> Value {
    json!({"name": name, "valueString": value})
}

async fn holding(router: &axum::Router) {
    let held = [
        json!({
            "resourceType": "Practitioner",
            "id": "pr1",
            "identifier": [{"system": "urn:staff", "value": "1234"}],
            "name": [{"family": "Stone", "given": ["Ada"]}],
        }),
        json!({
            "resourceType": "Practitioner",
            "id": "pr2",
            "identifier": [{"system": "urn:staff", "value": "5678"}],
            "name": [{"family": "Stone", "given": ["Bee"]}],
        }),
        json!({
            "resourceType": "Patient",
            "id": "pt1",
            "identifier": [{"system": "urn:nhs", "value": "9999"}],
            "name": [{"family": "Rivers"}],
        }),
    ];
    for resource in held {
        let kind = resource["resourceType"].as_str().expect("a type");
        let id = resource["id"].as_str().expect("an id");
        let (status, body) = ask(
            router,
            "PUT",
            &format!("/{kind}/{id}"),
            Some(resource.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
}

#[tokio::test]
async fn one_match_is_the_user() {
    let router = router();
    holding(&router).await;
    let (status, body) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(vec![named("identifier", "urn:staff|1234")])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a resource");
    assert_eq!(held["resourceType"], "Practitioner");
    assert_eq!(
        held["id"], "pr1",
        "the type and the id are what becomes the claim"
    );
}

#[tokio::test]
async fn a_user_of_another_type_is_found_too() {
    let router = router();
    holding(&router).await;
    let (status, body) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(vec![named("identifier", "urn:nhs|9999")])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a resource");
    assert_eq!(held["resourceType"], "Patient");
}

#[tokio::test]
async fn two_matches_are_refused_rather_than_guessed_between() {
    let router = router();
    holding(&router).await;
    let (status, body) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(vec![named("family", "Stone")])),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.contains("wrong person"),
        "the answer becomes a claim, and a wrong claim is a person reading \
         another person's record: {body}"
    );
}

#[tokio::test]
async fn no_match_is_not_found() {
    let router = router();
    holding(&router).await;
    let (status, _) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(vec![named("identifier", "urn:staff|nobody")])),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_lookup_may_name_the_type_it_wants() {
    let router = router();
    holding(&router).await;
    let (status, body) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(vec![
            named("resourceType", "Practitioner"),
            named("identifier", "urn:staff|1234"),
        ])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a resource");
    assert_eq!(held["resourceType"], "Practitioner");

    let (wrong, _) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(vec![
            named("resourceType", "Patient"),
            named("identifier", "urn:staff|1234"),
        ])),
    )
    .await;
    assert_eq!(
        wrong,
        StatusCode::NOT_FOUND,
        "the identity provider said which type it wanted"
    );
}

#[tokio::test]
async fn a_type_no_user_may_be_is_refused() {
    let router = router();
    let (status, body) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(vec![
            named("resourceType", "Observation"),
            named("code", "x"),
        ])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("not a type a user may be"), "{body}");
}

#[tokio::test]
async fn a_lookup_naming_no_parameter_is_refused() {
    let router = router();
    holding(&router).await;
    let (status, body) = ask(
        &router,
        "POST",
        "/$fhirUser-lookup",
        Some(looking(Vec::new())),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("every user this instance holds"),
        "a lookup that matched everyone would name whoever sorted first: {body}"
    );
}

#[tokio::test]
async fn the_operation_is_advertised() {
    let router = router();
    let (status, body) = ask(&router, "GET", "/metadata", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("fhirUser-lookup"),
        "an identity provider reads the statement to decide whether to use it"
    );
}
