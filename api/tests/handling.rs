use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn ask(app: &Service, method: &str, uri: &str, prefer: Option<&str>, body: &[u8]) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json");
    if let Some(prefer) = prefer {
        builder = builder.header("prefer", prefer);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn json(reply: &Reply) -> Value {
    serde_json::from_str(&reply.body).expect("a json body")
}

fn self_link(value: &Value) -> String {
    value["link"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["relation"] == "self")
        .unwrap()["url"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn ignored(value: &Value) -> Option<String> {
    value["entry"].as_array()?.iter().find_map(|entry| {
        (entry["search"]["mode"] == "outcome")
            .then(|| entry["resource"]["issue"][0]["diagnostics"].as_str())
            .flatten()
            .map(str::to_owned)
    })
}

async fn seed(app: &Service) {
    for id in ["h-1", "h-2"] {
        let body = serde_json::json!({"resourceType": "Patient", "id": id, "active": true})
            .to_string()
            .into_bytes();
        ask(app, "PUT", &format!("/Patient/{id}"), None, &body).await;
    }
}

#[tokio::test]
async fn an_unanswerable_parameter_is_refused_when_nothing_is_asked_for() {
    let app = service();
    seed(&app).await;
    let reply = ask(&app, "GET", "/Patient?nonesuch=x", None, b"").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
}

#[tokio::test]
async fn an_unanswerable_parameter_is_refused_under_strict_handling() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "GET",
        "/Patient?nonesuch=x",
        Some("handling=strict"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
}

#[tokio::test]
async fn a_lenient_search_drops_what_it_cannot_answer_and_names_it_back() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "GET",
        "/Patient?nonesuch=x&active=true",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    assert_eq!(value["total"], 2, "{}", reply.body);
    let told = ignored(&value).expect("an outcome entry names what was ignored");
    assert!(told.contains("nonesuch"), "{told}");
    let link = self_link(&value);
    assert!(
        !link.contains("nonesuch"),
        "the self link carries nothing that was ignored: {link}"
    );
    assert!(link.contains("active=true"), "{link}");
}

#[tokio::test]
async fn a_lenient_search_that_keeps_nothing_still_answers() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "GET",
        "/Patient?nonesuch=x",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    assert_eq!(value["total"], 2);
    assert_eq!(self_link(&value), "http://localhost/Patient");
}

#[tokio::test]
async fn a_lenient_search_still_refuses_a_value_that_does_not_parse() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "GET",
        "/Patient?birthdate=nonsense",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "a known parameter with a bad value is an error under either handling: {}",
        reply.body
    );
}

#[tokio::test]
async fn a_handling_value_that_names_neither_is_refused() {
    let app = service();
    let reply = ask(&app, "GET", "/Patient", Some("handling=maybe"), b"").await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("maybe"), "{}", reply.body);
}

#[tokio::test]
async fn the_posted_search_form_reads_the_handling_too() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "POST",
        "/Patient/_search?nonesuch=x",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(ignored(&json(&reply)).is_some(), "{}", reply.body);
}

#[tokio::test]
async fn a_system_search_reads_the_handling_too() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "GET",
        "/?_type=Patient&nonesuch=x",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(ignored(&json(&reply)).is_some(), "{}", reply.body);
}

#[tokio::test]
async fn a_compartment_search_reads_the_handling_too() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "GET",
        "/Patient/h-1/Observation?nonesuch=x",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(ignored(&json(&reply)).is_some(), "{}", reply.body);
}

#[tokio::test]
async fn everything_reads_the_handling_too() {
    let app = service();
    seed(&app).await;
    let strict = ask(
        &app,
        "GET",
        "/Patient/h-1/$everything?nonesuch=x",
        None,
        b"",
    )
    .await;
    assert_eq!(strict.status, StatusCode::BAD_REQUEST, "{}", strict.body);
    let reply = ask(
        &app,
        "GET",
        "/Patient/h-1/$everything?nonesuch=x",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let value = json(&reply);
    assert!(ignored(&value).is_some(), "{}", reply.body);
    assert!(!self_link(&value).contains("nonesuch"));
}

#[tokio::test]
async fn a_parameter_the_release_names_but_cannot_answer_is_dropped_too() {
    let app = service();
    seed(&app).await;
    let reply = ask(
        &app,
        "GET",
        "/Patient?_filter=name%20eq%20a",
        Some("handling=lenient"),
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let told = ignored(&json(&reply)).expect("an outcome entry");
    assert!(told.contains("_filter"), "{told}");
}
