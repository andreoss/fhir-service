use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{SecurityHeaders, Service};
use fhir_core::{FhirInstant, FhirVersion};
use std::sync::Arc;
use tower::ServiceExt;

fn service(headers: SecurityHeaders) -> axum::Router {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .with_security_headers(headers)
        .router()
}

async fn head_of(router: &axum::Router, uri: &str, proto: Option<&str>) -> Vec<(String, String)> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "localhost");
    if let Some(proto) = proto {
        builder = builder.header("x-forwarded-proto", proto);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).expect("a request"))
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn the_four_a_plain_request_gets() {
    let router = service(SecurityHeaders::default());
    let headers = head_of(&router, "/metadata", None).await;
    assert_eq!(value(&headers, "x-content-type-options"), Some("nosniff"));
    assert_eq!(value(&headers, "x-frame-options"), Some("DENY"));
    assert_eq!(value(&headers, "referrer-policy"), Some("no-referrer"));
    assert!(value(&headers, "content-security-policy").is_some());
}

#[tokio::test]
async fn hsts_is_sent_only_where_the_request_arrived_over_tls() {
    let router = service(SecurityHeaders::default());
    let plain = head_of(&router, "/metadata", None).await;
    assert_eq!(
        value(&plain, "strict-transport-security"),
        None,
        "asking a browser to remember a promise the deployment has not made \
         would lock it out of its own service"
    );
    let secured = head_of(&router, "/metadata", Some("https")).await;
    assert!(value(&secured, "strict-transport-security").is_some());
}

#[tokio::test]
async fn a_gateway_naming_http_gets_no_hsts() {
    let router = service(SecurityHeaders::default());
    let held = head_of(&router, "/metadata", Some("http")).await;
    assert_eq!(value(&held, "strict-transport-security"), None);
}

#[tokio::test]
async fn one_turned_off_is_absent_and_the_others_remain() {
    let router = service(SecurityHeaders::parse("frame-options=off").expect("a valid setting"));
    let headers = head_of(&router, "/metadata", None).await;
    assert_eq!(value(&headers, "x-frame-options"), None);
    assert_eq!(value(&headers, "x-content-type-options"), Some("nosniff"));
}

#[tokio::test]
async fn all_turned_off_sends_none_of_them() {
    let router = service(SecurityHeaders::parse("all=off").expect("a valid setting"));
    let headers = head_of(&router, "/metadata", Some("https")).await;
    for name in [
        "x-content-type-options",
        "x-frame-options",
        "referrer-policy",
        "content-security-policy",
        "strict-transport-security",
    ] {
        assert_eq!(value(&headers, name), None, "{name} is off");
    }
    assert!(
        value(&headers, "content-type").is_some(),
        "and the answer is still an answer"
    );
}

#[tokio::test]
async fn a_policy_the_operator_wrote_is_the_one_sent() {
    let router = service(
        SecurityHeaders::parse(
            "content-security-policy=default-src 'self'; frame-options=SAMEORIGIN",
        )
        .expect("a valid setting"),
    );
    let headers = head_of(&router, "/metadata", None).await;
    assert_eq!(
        value(&headers, "content-security-policy"),
        Some("default-src 'self'")
    );
    assert_eq!(value(&headers, "x-frame-options"), Some("SAMEORIGIN"));
}

#[tokio::test]
async fn they_are_on_a_refusal_too() {
    let router = service(SecurityHeaders::default());
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/Nonsense/1")
                .header("host", "localhost")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("an answer");
    assert!(response.status().is_client_error());
    assert!(
        response.headers().contains_key("x-content-type-options"),
        "an error body is a body a browser may sniff too"
    );
}
