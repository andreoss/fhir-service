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
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn request(app: &Service, method: &str, uri: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let mut builder = Request::builder().method(method).uri(uri).header("host", "localhost");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
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

async fn post(app: &Service, bundle: &Value) -> (StatusCode, Value) {
    let reply = request(app, "POST", "/", &[], bundle.to_string().as_bytes()).await;
    let value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
    (reply.status, value)
}

fn patient(id: &str, active: bool) -> Value {
    json!({"resourceType": "Patient", "id": id, "active": active})
}

fn bundle(kind: &str, entries: Vec<Value>) -> Value {
    json!({"resourceType": "Bundle", "type": kind, "entry": entries})
}

fn write(method: &str, url: &str, resource: Value) -> Value {
    json!({"resource": resource, "request": {"method": method, "url": url}})
}

fn plain(method: &str, url: &str) -> Value {
    json!({"request": {"method": method, "url": url}})
}

fn statuses(value: &Value) -> Vec<String> {
    value["entry"]
        .as_array()
        .expect("a response bundle carries entries")
        .iter()
        .map(|entry| entry["response"]["status"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn a_transaction_applies_every_entry() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("tx-1", true)),
            write("POST", "Patient", patient("tx-2", false)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["resourceType"], "Bundle");
    assert_eq!(body["type"], "transaction-response");
    assert_eq!(statuses(&body), vec!["201 Created".to_owned(), "201 Created".to_owned()]);
    assert_eq!(body["entry"][0]["response"]["location"], "Patient/tx-1/_history/1");
    assert_eq!(body["entry"][0]["response"]["etag"], "W/\"1\"");
    assert_eq!(body["entry"][0]["resource"]["id"], "tx-1");
    assert_eq!(request(&app, "GET", "/Patient/tx-2", &[], &[]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn one_failing_entry_rolls_the_whole_transaction_back() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("tx-3", true)),
            write("POST", "Nonesuch", patient("tx-4", true)),
            write("POST", "Patient", patient("tx-5", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["resourceType"], "OperationOutcome");
    assert_eq!(request(&app, "GET", "/Patient/tx-3", &[], &[]).await.status, StatusCode::NOT_FOUND);
    assert_eq!(request(&app, "GET", "/Patient/tx-5", &[], &[]).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_transaction_that_fails_late_leaves_nothing_behind() {
    let app = service();
    request(&app, "POST", "/Patient", &[], patient("tx-6", true).to_string().as_bytes()).await;
    let sent = bundle(
        "transaction",
        vec![
            write("PUT", "Patient/tx-6", patient("tx-6", false)),
            write("POST", "Patient", patient("tx-6", true)),
        ],
    );
    let (status, _) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let after = request(&app, "GET", "/Patient/tx-6", &[], &[]).await;
    let stored: Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(stored["active"], true);
    assert_eq!(stored["meta"]["versionId"], "1");
}

#[tokio::test]
async fn an_unknown_bundle_type_is_rejected() {
    let app = service();
    let (status, body) = post(&app, &bundle("collection", Vec::new())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn a_body_that_is_not_a_bundle_is_rejected() {
    let app = service();
    let (status, _) = post(&app, &patient("tx-7", true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_nested_bundle_entry_is_rejected() {
    let app = service();
    let inner = bundle("batch", vec![plain("GET", "Patient/tx-8")]);
    let (status, _) = post(&app, &bundle("transaction", vec![write("POST", "", inner)])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_batch_reports_one_outcome_per_entry() {
    let app = service();
    let sent = bundle(
        "batch",
        vec![
            write("POST", "Patient", patient("ba-1", true)),
            write("POST", "Nonesuch", patient("ba-2", true)),
            write("POST", "Patient", patient("ba-3", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["type"], "batch-response");
    let reported = statuses(&body);
    assert_eq!(reported[0], "201 Created");
    assert!(reported[1].starts_with("400"));
    assert_eq!(reported[2], "201 Created");
    assert_eq!(body["entry"][1]["outcome"]["resourceType"], "OperationOutcome");
    assert!(body["entry"][1].get("resource").is_none());
    assert_eq!(request(&app, "GET", "/Patient/ba-1", &[], &[]).await.status, StatusCode::OK);
    assert_eq!(request(&app, "GET", "/Patient/ba-3", &[], &[]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_malformed_batch_entry_fails_only_itself() {
    let app = service();
    let sent = bundle(
        "batch",
        vec![
            json!({"request": {"method": "SING", "url": "Patient"}}),
            write("POST", "Patient", patient("ba-4", true)),
        ],
    );
    let (status, body) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::OK);
    assert!(statuses(&body)[0].starts_with("400"));
    assert_eq!(statuses(&body)[1], "201 Created");
    assert_eq!(request(&app, "GET", "/Patient/ba-4", &[], &[]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_malformed_transaction_entry_fails_the_bundle() {
    let app = service();
    let sent = bundle(
        "transaction",
        vec![
            write("POST", "Patient", patient("ba-5", true)),
            json!({"request": {"method": "SING", "url": "Patient"}}),
        ],
    );
    let (status, _) = post(&app, &sent).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(request(&app, "GET", "/Patient/ba-5", &[], &[]).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_batch_leaves_earlier_entries_in_place_when_a_later_one_fails() {
    let app = service();
    request(&app, "POST", "/Patient", &[], patient("ba-6", true).to_string().as_bytes()).await;
    let sent = bundle(
        "batch",
        vec![
            write("PUT", "Patient/ba-6", patient("ba-6", false)),
            write("POST", "Patient", patient("ba-6", true)),
        ],
    );
    let (_, body) = post(&app, &sent).await;
    assert_eq!(statuses(&body)[0], "200 OK");
    assert!(statuses(&body)[1].starts_with("409"));
    let after = request(&app, "GET", "/Patient/ba-6", &[], &[]).await;
    let stored: Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(stored["active"], false);
    assert_eq!(stored["meta"]["versionId"], "2");
}
