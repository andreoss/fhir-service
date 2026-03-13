use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Forwarding, References, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const PROXIED: &str = "https://fhir.example.org";

fn service(forwarding: Forwarding, references: References) -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .behind_proxy(forwarding)
        .normalising(references)
        .router()
}

async fn ask(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    proxied: bool,
) -> (StatusCode, Vec<(String, String)>, String) {
    let held = match &body {
        Some(body) => Body::from(serde_json::to_vec(body).expect("a body")),
        None => Body::empty(),
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:8080")
        .header("content-type", "application/fhir+json");
    if proxied {
        builder = builder
            .header("x-forwarded-host", "fhir.example.org")
            .header("x-forwarded-proto", "https");
    }
    let response = router
        .clone()
        .oneshot(builder.body(held).expect("a request"))
        .await
        .expect("an answer");
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| value.as_str())
}

fn patient() -> Value {
    json!({"resourceType": "Patient", "id": "p1", "active": true})
}

fn observation(subject: &str) -> Value {
    json!({
        "resourceType": "Observation",
        "id": "ob1",
        "status": "final",
        "code": {"text": "probe"},
        "subject": {"reference": subject},
    })
}

#[tokio::test]
async fn an_untrusting_instance_hands_back_its_own_address() {
    let router = service(Forwarding::untrusted(), References::as_written());
    let (status, headers, _) = ask(&router, "PUT", "/Patient/p1", Some(patient()), true).await;
    assert_eq!(status, StatusCode::CREATED);
    let location = header(&headers, "content-location").expect("a location");
    assert!(
        location.starts_with("http://127.0.0.1:8080/"),
        "a client may write those headers, and an instance that has not been \
         told about a proxy does not read them: {location}"
    );
}

#[tokio::test]
async fn an_instance_behind_a_proxy_hands_back_the_address_the_client_asked_at() {
    let router = service(Forwarding::trusted(), References::as_written());
    let (status, headers, _) = ask(&router, "PUT", "/Patient/p1", Some(patient()), true).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        header(&headers, "content-location"),
        Some("https://fhir.example.org/Patient/p1/_history/1"),
        "the scheme and the authority, both of which were wrong before"
    );
}

#[tokio::test]
async fn the_paging_links_are_on_the_address_the_client_asked_at() {
    let router = service(Forwarding::trusted(), References::as_written());
    for index in 0..3 {
        ask(
            &router,
            "PUT",
            &format!("/Patient/p{index}"),
            Some(json!({"resourceType": "Patient", "id": format!("p{index}"), "active": true})),
            true,
        )
        .await;
    }
    let (_, _, body) = ask(&router, "GET", "/Patient?_count=1", None, true).await;
    let held: Value = serde_json::from_str(&body).expect("a bundle");
    for link in held["link"].as_array().expect("links") {
        let url = link["url"].as_str().expect("a url");
        assert!(url.starts_with(PROXIED), "{url}");
    }
    let full = held["entry"][0]["fullUrl"].as_str().expect("a full url");
    assert!(full.starts_with(PROXIED), "{full}");
}

#[tokio::test]
async fn the_capability_statement_names_the_address_the_client_asked_at() {
    let router = service(Forwarding::trusted(), References::as_written());
    let (_, _, body) = ask(&router, "GET", "/metadata", None, true).await;
    assert!(
        body.contains(PROXIED),
        "a statement is read to learn where to send things: {}",
        &body[..body.len().min(400)]
    );
}

#[tokio::test]
async fn a_reference_to_this_server_is_stored_relative_and_answered_absolute() {
    let router = service(Forwarding::trusted(), References::normalised(["url"]));
    ask(&router, "PUT", "/Patient/p1", Some(patient()), true).await;
    let (status, _, _) = ask(
        &router,
        "PUT",
        "/Observation/ob1",
        Some(observation("https://fhir.example.org/Patient/p1")),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, _, body) = ask(&router, "GET", "/Observation/ob1", None, true).await;
    let held: Value = serde_json::from_str(&body).expect("a resource");
    assert_eq!(
        held["subject"]["reference"], "https://fhir.example.org/Patient/p1",
        "the client reads back what it wrote"
    );

    let (_, _, elsewhere) = ask(&router, "GET", "/Observation/ob1", None, false).await;
    let held: Value = serde_json::from_str(&elsewhere).expect("a resource");
    assert_eq!(
        held["subject"]["reference"], "http://127.0.0.1:8080/Patient/p1",
        "and a client at another address reads one it can follow, which is the \
         point of storing it relative"
    );
}

#[tokio::test]
async fn a_reference_to_another_server_is_left_alone() {
    let router = service(Forwarding::trusted(), References::normalised(["url"]));
    ask(
        &router,
        "PUT",
        "/Observation/ob1",
        Some(observation("https://other.example.org/Patient/p9")),
        true,
    )
    .await;
    let (_, _, body) = ask(&router, "GET", "/Observation/ob1", None, true).await;
    let held: Value = serde_json::from_str(&body).expect("a resource");
    assert_eq!(
        held["subject"]["reference"],
        "https://other.example.org/Patient/p9"
    );
}

#[tokio::test]
async fn an_instance_that_was_not_asked_stores_what_it_was_given() {
    let router = service(Forwarding::trusted(), References::as_written());
    ask(
        &router,
        "PUT",
        "/Observation/ob1",
        Some(observation("Patient/p1")),
        true,
    )
    .await;
    let (_, _, body) = ask(&router, "GET", "/Observation/ob1", None, true).await;
    let held: Value = serde_json::from_str(&body).expect("a resource");
    assert_eq!(
        held["subject"]["reference"], "Patient/p1",
        "which is what this build did before the setting existed"
    );
}

#[tokio::test]
async fn a_search_bundle_carries_absolute_references_too() {
    let router = service(Forwarding::trusted(), References::normalised(["url"]));
    ask(
        &router,
        "PUT",
        "/Observation/ob1",
        Some(observation("Patient/p1")),
        true,
    )
    .await;
    let (_, _, body) = ask(&router, "GET", "/Observation", None, true).await;
    let held: Value = serde_json::from_str(&body).expect("a bundle");
    assert_eq!(
        held["entry"][0]["resource"]["subject"]["reference"],
        "https://fhir.example.org/Patient/p1"
    );
}
