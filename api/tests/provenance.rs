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

async fn ask(
    app: &Service,
    method: &str,
    uri: &str,
    provenance: Option<&str>,
    content_type: &str,
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", content_type);
    if let Some(carried) = provenance {
        builder = builder.header("x-provenance", carried);
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

fn provenance() -> String {
    json!({
        "resourceType": "Provenance",
        "recorded": "2026-09-06T04:00:00Z",
        "agent": [{"who": {"display": "a clinician"}}]
    })
    .to_string()
}

fn patient(id: &str) -> Vec<u8> {
    json!({"resourceType": "Patient", "id": id, "active": true})
        .to_string()
        .into_bytes()
}

async fn stored_provenances(app: &Service) -> Vec<Value> {
    let reply = ask(
        app,
        "GET",
        "/Provenance",
        None,
        "application/fhir+json",
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let bundle: Value = serde_json::from_str(&reply.body).unwrap();
    bundle["entry"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["resource"].clone())
                .collect()
        })
        .unwrap_or_default()
}

fn targets(resource: &Value) -> Vec<String> {
    resource["target"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| target["reference"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_create_that_carries_a_provenance_stores_it_against_the_version_written() {
    let app = service();
    let reply = ask(
        &app,
        "POST",
        "/Patient",
        Some(&provenance()),
        "application/fhir+json",
        &patient("one"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let held = stored_provenances(&app).await;
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(targets(&held[0]), vec!["Patient/one/_history/1".to_owned()]);
}

#[tokio::test]
async fn an_update_that_carries_a_provenance_names_the_version_it_wrote() {
    let app = service();
    ask(
        &app,
        "PUT",
        "/Patient/one",
        None,
        "application/fhir+json",
        &patient("one"),
    )
    .await;
    let amended = json!({"resourceType": "Patient", "id": "one", "active": false})
        .to_string()
        .into_bytes();
    let reply = ask(
        &app,
        "PUT",
        "/Patient/one",
        Some(&provenance()),
        "application/fhir+json",
        &amended,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let held = stored_provenances(&app).await;
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(targets(&held[0]), vec!["Patient/one/_history/2".to_owned()]);
}

#[tokio::test]
async fn a_patch_that_carries_a_provenance_names_the_version_it_wrote() {
    let app = service();
    ask(
        &app,
        "PUT",
        "/Patient/one",
        None,
        "application/fhir+json",
        &patient("one"),
    )
    .await;
    let patch = json!([{"op": "replace", "path": "/active", "value": false}])
        .to_string()
        .into_bytes();
    let reply = ask(
        &app,
        "PATCH",
        "/Patient/one",
        Some(&provenance()),
        "application/json-patch+json",
        &patch,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let held = stored_provenances(&app).await;
    assert_eq!(targets(&held[0]), vec!["Patient/one/_history/2".to_owned()]);
}

#[tokio::test]
async fn a_transaction_records_one_provenance_naming_every_version_it_wrote() {
    let app = service();
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": [
            {"resource": {"resourceType": "Patient", "id": "one", "active": true},
             "request": {"method": "PUT", "url": "Patient/one"}},
            {"resource": {"resourceType": "Patient", "id": "two", "active": true},
             "request": {"method": "PUT", "url": "Patient/two"}}
        ]
    })
    .to_string()
    .into_bytes();
    let reply = ask(
        &app,
        "POST",
        "/",
        Some(&provenance()),
        "application/fhir+json",
        &bundle,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let held = stored_provenances(&app).await;
    assert_eq!(held.len(), 1, "{held:?}");
    let mut named = targets(&held[0]);
    named.sort();
    assert_eq!(
        named,
        vec![
            "Patient/one/_history/1".to_owned(),
            "Patient/two/_history/1".to_owned()
        ]
    );
}

#[tokio::test]
async fn a_transaction_that_rolls_back_stores_no_provenance() {
    let app = service();
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": [
            {"resource": {"resourceType": "Patient", "id": "one", "active": true},
             "request": {"method": "PUT", "url": "Patient/one"}},
            {"request": {"method": "GET", "url": "Patient/absent"}}
        ]
    })
    .to_string()
    .into_bytes();
    let reply = ask(
        &app,
        "POST",
        "/",
        Some(&provenance()),
        "application/fhir+json",
        &bundle,
    )
    .await;
    assert!(reply.status.is_client_error(), "{}", reply.status);
    assert!(stored_provenances(&app).await.is_empty());
}

#[tokio::test]
async fn a_write_that_fails_stores_no_provenance() {
    let app = service();
    let reply = ask(
        &app,
        "PUT",
        "/Patient/one",
        Some(&provenance()),
        "application/fhir+json",
        &json!({"resourceType": "Observation", "id": "one"})
            .to_string()
            .into_bytes(),
    )
    .await;
    assert!(reply.status.is_client_error(), "{}", reply.status);
    assert!(stored_provenances(&app).await.is_empty());
}

#[tokio::test]
async fn a_header_that_is_not_a_provenance_is_refused_before_the_write() {
    let app = service();
    let reply = ask(
        &app,
        "POST",
        "/Patient",
        Some(&json!({"resourceType": "Patient"}).to_string()),
        "application/fhir+json",
        &patient("one"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("not a Provenance"), "{}", reply.body);
    let reply = ask(
        &app,
        "GET",
        "/Patient/one",
        None,
        "application/fhir+json",
        b"",
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_header_that_already_names_a_target_is_refused() {
    let app = service();
    let carried = json!({
        "resourceType": "Provenance",
        "recorded": "2026-09-06T04:00:00Z",
        "target": [{"reference": "Patient/other"}],
        "agent": [{"who": {"display": "a clinician"}}]
    })
    .to_string();
    let reply = ask(
        &app,
        "POST",
        "/Patient",
        Some(&carried),
        "application/fhir+json",
        &patient("one"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(
        reply.body.contains("already names a target"),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn a_write_without_the_header_stores_no_provenance() {
    let app = service();
    ask(
        &app,
        "POST",
        "/Patient",
        None,
        "application/fhir+json",
        &patient("one"),
    )
    .await;
    assert!(stored_provenances(&app).await.is_empty());
}
