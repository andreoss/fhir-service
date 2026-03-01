use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn ask(
    app: &Service,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn header<'a>(reply: &'a Reply, name: &str) -> &'a str {
    reply
        .headers
        .iter()
        .find(|(held, _)| held.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

async fn created(app: &Service) -> (String, String) {
    let reply = ask(
        app,
        "POST",
        "/Patient",
        &[],
        br#"{"resourceType":"Patient","active":true}"#,
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let body: serde_json::Value = serde_json::from_str(&reply.body).expect("body is json");
    let id = body
        .get("id")
        .and_then(|id| id.as_str())
        .expect("id in body")
        .to_owned();
    let etag = header(&reply, "etag").to_owned();
    (format!("/Patient/{id}"), etag)
}

#[tokio::test]
async fn a_read_carrying_the_etag_it_was_given_answers_not_modified() {
    let app = service();
    let (path, etag) = created(&app).await;
    assert_eq!(etag, "W/\"1\"");

    let reply = ask(&app, "GET", &path, &[("if-none-match", &etag)], b"").await;

    assert_eq!(reply.status, StatusCode::NOT_MODIFIED, "{}", reply.body);
    assert!(reply.body.is_empty());
}

#[tokio::test]
async fn a_not_modified_answer_carries_the_etag_and_the_last_write() {
    let app = service();
    let (path, etag) = created(&app).await;

    let reply = ask(&app, "GET", &path, &[("if-none-match", &etag)], b"").await;

    assert_eq!(header(&reply, "etag"), etag);
    assert_eq!(
        header(&reply, "last-modified"),
        "Sun, 06 Sep 2026 04:00:00 GMT"
    );
    assert_eq!(header(&reply, "cache-control"), "no-cache");
}

#[tokio::test]
async fn a_read_carrying_a_stale_etag_answers_the_resource() {
    let app = service();
    let (path, _) = created(&app).await;

    let reply = ask(&app, "GET", &path, &[("if-none-match", "W/\"9\"")], b"").await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains("\"Patient\""));
}

#[tokio::test]
async fn a_read_carrying_the_wildcard_answers_not_modified() {
    let app = service();
    let (path, _) = created(&app).await;

    let reply = ask(&app, "GET", &path, &[("if-none-match", "*")], b"").await;

    assert_eq!(reply.status, StatusCode::NOT_MODIFIED, "{}", reply.body);
}

#[tokio::test]
async fn a_read_since_the_write_answers_not_modified() {
    let app = service();
    let (path, _) = created(&app).await;

    let reply = ask(
        &app,
        "GET",
        &path,
        &[("if-modified-since", "Sun, 06 Sep 2026 04:00:00 GMT")],
        b"",
    )
    .await;

    assert_eq!(reply.status, StatusCode::NOT_MODIFIED, "{}", reply.body);
}

#[tokio::test]
async fn a_read_since_before_the_write_answers_the_resource() {
    let app = service();
    let (path, _) = created(&app).await;

    let reply = ask(
        &app,
        "GET",
        &path,
        &[("if-modified-since", "Sun, 06 Sep 2026 03:59:59 GMT")],
        b"",
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

#[tokio::test]
async fn a_conditional_read_of_what_is_not_there_is_still_a_miss() {
    let app = service();

    let reply = ask(
        &app,
        "GET",
        "/Patient/absent",
        &[("if-none-match", "W/\"1\"")],
        b"",
    )
    .await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
}

#[tokio::test]
async fn a_version_that_is_not_one_answers_the_resource() {
    let app = service();
    let (path, _) = created(&app).await;

    let reply = ask(
        &app,
        "GET",
        &path,
        &[("if-none-match", "W/\"not-a-version\"")],
        b"",
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}
