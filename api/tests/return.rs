use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
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

fn patient(id: &str, active: bool) -> Vec<u8> {
    format!(r#"{{"resourceType":"Patient","id":"{id}","active":{active}}}"#).into_bytes()
}

fn patch() -> Vec<u8> {
    r#"[{"op":"replace","path":"/active","value":false}]"#.into()
}

fn outcome(body: &str) -> Value {
    serde_json::from_str(body).expect("an outcome is json")
}

#[tokio::test]
async fn a_create_asked_for_minimal_answers_with_no_body() {
    let app = service();
    let reply = ask(
        &app,
        "POST",
        "/Patient",
        Some("return=minimal"),
        &patient("rt-min", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert!(reply.body.is_empty(), "{}", reply.body);
    assert!(header(&reply, "location").ends_with("/Patient/rt-min/_history/1"));
    assert_eq!(header(&reply, "etag"), "W/\"1\"");
    assert!(header(&reply, "last-modified").ends_with("GMT"));
}

#[tokio::test]
async fn a_create_asked_for_representation_answers_with_the_resource() {
    let app = service();
    let reply = ask(
        &app,
        "POST",
        "/Patient",
        Some("return=representation"),
        &patient("rt-rep", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let body: Value = serde_json::from_str(&reply.body).expect("the resource is json");
    assert_eq!(body["resourceType"], "Patient");
    assert_eq!(body["id"], "rt-rep");
}

#[tokio::test]
async fn a_create_asked_for_an_outcome_answers_with_one() {
    let app = service();
    let reply = ask(
        &app,
        "POST",
        "/Patient",
        Some("return=OperationOutcome"),
        &patient("rt-out", true),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let body = outcome(&reply.body);
    assert_eq!(body["resourceType"], "OperationOutcome");
    assert_eq!(body["issue"][0]["severity"], "information");
    assert_eq!(body["issue"][0]["code"], "informational");
    assert!(header(&reply, "location").ends_with("/Patient/rt-out/_history/1"));
}

#[tokio::test]
async fn an_update_asked_for_minimal_answers_with_no_body() {
    let app = service();
    let created = ask(&app, "POST", "/Patient", None, &patient("rt-upd", true)).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let reply = ask(
        &app,
        "PUT",
        "/Patient/rt-upd",
        Some("return=minimal"),
        &patient("rt-upd", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.is_empty(), "{}", reply.body);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
}

#[tokio::test]
async fn a_patch_asked_for_minimal_answers_with_no_body() {
    let app = service();
    let created = ask(&app, "POST", "/Patient", None, &patient("rt-pat", true)).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let reply = ask(
        &app,
        "PATCH",
        "/Patient/rt-pat",
        Some("return=minimal"),
        &patch(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.is_empty(), "{}", reply.body);
    assert_eq!(header(&reply, "etag"), "W/\"2\"");
}

#[tokio::test]
async fn a_conditional_update_asked_for_minimal_answers_with_no_body() {
    let app = service();
    let created = ask(&app, "POST", "/Patient", None, &patient("rt-con", true)).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let reply = ask(
        &app,
        "PUT",
        "/Patient?active=true",
        Some("return=minimal"),
        &patient("rt-con", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.is_empty(), "{}", reply.body);
}

#[tokio::test]
async fn a_transaction_asked_for_minimal_answers_with_only_the_outcomes() {
    let app = service();
    let sent = json!({
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": [
            {"resource": {"resourceType": "Patient", "id": "rt-tx1", "active": true},
             "request": {"method": "POST", "url": "Patient"}},
            {"resource": {"resourceType": "Patient", "id": "rt-tx2", "active": false},
             "request": {"method": "POST", "url": "Patient"}}
        ]
    });
    let reply = ask(
        &app,
        "POST",
        "/",
        Some("return=minimal"),
        sent.to_string().as_bytes(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let body: Value = serde_json::from_str(&reply.body).expect("the bundle is json");
    assert_eq!(body["type"], "transaction-response");
    let entries = body["entry"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert_eq!(entry["response"]["status"], "201 Created");
        assert!(entry.get("resource").is_none(), "{entry}");
    }
}

#[tokio::test]
async fn a_transaction_asked_for_representation_carries_the_resources() {
    let app = service();
    let sent = json!({
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": [
            {"resource": {"resourceType": "Patient", "id": "rt-tx3", "active": true},
             "request": {"method": "POST", "url": "Patient"}}
        ]
    });
    let reply = ask(
        &app,
        "POST",
        "/",
        Some("return=representation"),
        sent.to_string().as_bytes(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let body: Value = serde_json::from_str(&reply.body).expect("the bundle is json");
    assert_eq!(body["entry"][0]["resource"]["id"], "rt-tx3");
}

#[tokio::test]
async fn a_preference_the_server_does_not_carry_is_ignored() {
    let app = service();
    for (index, ignored) in ["return=nonsense", "handling=lenient", "nonsense"]
        .iter()
        .enumerate()
    {
        let reply = ask(
            &app,
            "POST",
            "/Patient",
            Some(ignored),
            &patient(&format!("rt-ign{index}"), true),
        )
        .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{ignored}");
        assert!(reply.body.contains("Patient"), "{ignored} {}", reply.body);
    }
}

#[tokio::test]
async fn a_failure_answers_with_its_outcome_whatever_the_preference() {
    let app = service();
    let reply = ask(
        &app,
        "POST",
        "/Patient",
        Some("return=minimal"),
        br#"{"resourceType":"Observation","id":"rt-bad"}"#,
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(outcome(&reply.body)["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn a_read_asked_for_minimal_still_answers_the_resource() {
    let app = service();
    let created = ask(&app, "POST", "/Patient", None, &patient("rt-read", true)).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let reply = ask(&app, "GET", "/Patient/rt-read", Some("return=minimal"), &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("rt-read"), "{}", reply.body);
}
