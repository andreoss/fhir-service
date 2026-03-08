use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Capabilities, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service(capabilities: Capabilities) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).with_capabilities(capabilities)
}

async fn ask(app: &Service, method: &str, uri: &str, body: &[u8]) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn links(body: &str) -> Vec<(String, String)> {
    let bundle: Value = serde_json::from_str(body).expect("a bundle");
    bundle["link"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|link| {
            (
                link["relation"].as_str().unwrap_or_default().to_owned(),
                link["url"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn named<'a>(held: &'a [(String, String)], relation: &str) -> Option<&'a str> {
    held.iter()
        .find(|(name, _)| name == relation)
        .map(|(_, url)| url.as_str())
}

fn relations(held: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = held.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    names
}

fn path_of(url: &str) -> String {
    url.split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, path)| format!("/{path}"))
        .unwrap_or_else(|| url.to_owned())
}

async fn seeded(count: usize) -> Service {
    let app = service(Capabilities::default());
    for index in 0..count {
        let body = json!({"resourceType": "Patient", "id": format!("pg-{index}"), "active": true})
            .to_string()
            .into_bytes();
        ask(&app, "PUT", &format!("/Patient/pg-{index}"), &body).await;
    }
    app
}

#[tokio::test]
async fn the_first_page_carries_self_next_and_last() {
    let app = seeded(7).await;
    let (status, body) = ask(&app, "GET", "/Patient?_count=3", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = links(&body);
    assert_eq!(
        relations(&held),
        vec!["last".to_owned(), "next".to_owned(), "self".to_owned()],
        "{body}"
    );
}

#[tokio::test]
async fn a_middle_page_carries_every_link() {
    let app = seeded(7).await;
    let (_, first) = ask(&app, "GET", "/Patient?_count=3", b"").await;
    let next = path_of(named(&links(&first), "next").expect("a next link"));
    let (status, body) = ask(&app, "GET", &next, b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = links(&body);
    assert_eq!(
        relations(&held),
        vec![
            "first".to_owned(),
            "last".to_owned(),
            "next".to_owned(),
            "previous".to_owned(),
            "self".to_owned()
        ],
        "{body}"
    );
}

#[tokio::test]
async fn the_last_page_carries_no_next_and_no_last() {
    let app = seeded(7).await;
    let (_, first) = ask(&app, "GET", "/Patient?_count=3", b"").await;
    let mut at = path_of(named(&links(&first), "next").expect("a next link"));
    let mut body;
    loop {
        let held = ask(&app, "GET", &at, b"").await;
        body = held.1;
        match named(&links(&body), "next") {
            Some(next) => at = path_of(next),
            None => break,
        }
    }
    let held = links(&body);
    assert_eq!(
        relations(&held),
        vec!["first".to_owned(), "previous".to_owned(), "self".to_owned()],
        "{body}"
    );
}

#[tokio::test]
async fn the_first_link_returns_to_the_first_page() {
    let app = seeded(7).await;
    let (_, first) = ask(&app, "GET", "/Patient?_count=3", b"").await;
    let next = path_of(named(&links(&first), "next").expect("a next link"));
    let (_, second) = ask(&app, "GET", &next, b"").await;
    let back = path_of(named(&links(&second), "first").expect("a first link"));
    let (_, again) = ask(&app, "GET", &back, b"").await;
    let ids = |body: &str| -> Vec<String> {
        let bundle: Value = serde_json::from_str(body).unwrap();
        bundle["entry"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["resource"]["id"].as_str().map(str::to_owned))
            .collect()
    };
    assert_eq!(ids(&again), ids(&first), "{again}");
}

#[tokio::test]
async fn a_page_that_holds_everything_carries_only_self() {
    let app = seeded(2).await;
    let (_, body) = ask(&app, "GET", "/Patient?_count=10", b"").await;
    assert_eq!(relations(&links(&body)), vec!["self".to_owned()], "{body}");
}

#[tokio::test]
async fn a_search_that_counts_nothing_carries_no_last() {
    let app = seeded(7).await;
    let (_, body) = ask(&app, "GET", "/Patient?_count=3&_total=none", b"").await;
    let held = relations(&links(&body));
    assert!(!held.contains(&"last".to_owned()), "{body}");
}

#[tokio::test]
async fn an_include_that_reaches_its_bound_says_so() {
    let app = service(Capabilities {
        include_depth: 1,
        ..Capabilities::default()
    });
    
    for index in 0..4 {
        let mut body = json!({"resourceType": "Organization", "id": format!("org-{index}")});
        if index < 3 {
            body["partOf"] = json!({"reference": format!("Organization/org-{}", index + 1)});
        }
        let held = body.to_string().into_bytes();
        ask(&app, "PUT", &format!("/Organization/org-{index}"), &held).await;
    }
    let (status, body) = ask(
        &app,
        "GET",
        "/Organization?_id=org-0&_include:iterate=Organization:partof",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bundle: Value = serde_json::from_str(&body).unwrap();
    let told = bundle["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|entry| entry["search"]["mode"] == "outcome");
    assert!(told, "the bound is named in an outcome entry: {body}");
}

#[tokio::test]
async fn an_include_that_finishes_says_nothing() {
    let app = service(Capabilities::default());
    for index in 0..2 {
        let mut body = json!({"resourceType": "Organization", "id": format!("ok-{index}")});
        if index < 1 {
            body["partOf"] = json!({"reference": format!("Organization/ok-{}", index + 1)});
        }
        let held = body.to_string().into_bytes();
        ask(&app, "PUT", &format!("/Organization/ok-{index}"), &held).await;
    }
    let (_, body) = ask(
        &app,
        "GET",
        "/Organization?_id=ok-0&_include:iterate=Organization:partof",
        b"",
    )
    .await;
    let bundle: Value = serde_json::from_str(&body).unwrap();
    let told = bundle["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|entry| entry["search"]["mode"] == "outcome");
    assert!(!told, "{body}");
}
