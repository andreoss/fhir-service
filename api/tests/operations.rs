use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
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
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Ok(())),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
}

async fn request(app: &Service, method: &str, uri: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn json(reply: &Reply) -> Value {
    serde_json::from_str(&reply.body).expect("body must be json")
}

fn conversion(collection: &str, root: &str, data: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "inputData", "valueString": data},
            {"name": "inputDataType", "valueString": "hl7v2"},
            {"name": "templateCollectionReference", "valueString": collection},
            {"name": "rootTemplate", "valueString": root}
        ]
    }))
    .unwrap()
}

#[tokio::test]
async fn convert_data_renders_the_named_template() {
    let app = service();
    let body = conversion(
        fhir_core::convert::DEFAULT_COLLECTION,
        "Patient",
        "PID|1||pt-1||Ann^Bea||19800401|female",
    );
    let reply = request(&app, "POST", "/$convert-data", &body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let value = json(&reply);
    assert_eq!(value["resourceType"], "Patient");
    assert_eq!(value["id"], "pt-1");
    assert_eq!(value["name"][0]["family"], "Ann");
}

#[tokio::test]
async fn convert_data_persists_nothing() {
    let app = service();
    let body = conversion(
        fhir_core::convert::DEFAULT_COLLECTION,
        "Patient",
        "PID|1||pt-2||Cyd",
    );
    assert_eq!(request(&app, "POST", "/$convert-data", &body).await.status, StatusCode::OK);
    let read = request(&app, "GET", "/Patient/pt-2", &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
    let searched = request(&app, "GET", "/Patient", &[]).await;
    assert_eq!(json(&searched)["total"], 0);
}

#[tokio::test]
async fn a_collection_outside_the_registry_is_refused() {
    let app = service();
    let body = conversion("urn:collection:other", "Patient", "PID|1||pt-3");
    let reply = request(&app, "POST", "/$convert-data", &body).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(json(&reply)["issue"][0]["code"], "forbidden");
}

#[tokio::test]
async fn a_conversion_missing_its_input_is_refused() {
    let app = service();
    let empty = serde_json::to_vec(&serde_json::json!({"resourceType": "Parameters"})).unwrap();
    let reply = request(&app, "POST", "/$convert-data", &empty).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&reply)["issue"][0]["code"], "invalid");
    let malformed = request(&app, "POST", "/$convert-data", b"not json").await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unknown_template_and_input_form_are_refused() {
    let app = service();
    let unknown = conversion(fhir_core::convert::DEFAULT_COLLECTION, "Nonesuch", "PID|1");
    assert_eq!(
        request(&app, "POST", "/$convert-data", &unknown).await.status,
        StatusCode::BAD_REQUEST
    );
    let form = serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "inputData", "valueString": "x"},
            {"name": "inputDataType", "valueString": "ccda"},
            {"name": "templateCollectionReference", "valueString": fhir_core::convert::DEFAULT_COLLECTION},
            {"name": "rootTemplate", "valueString": "Patient"}
        ]
    }))
    .unwrap();
    let reply = request(&app, "POST", "/$convert-data", &form).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&reply)["issue"][0]["code"], "not-supported");
}

async fn create_patient(app: &Service, id: &str) {
    let body = format!(r#"{{"resourceType":"Patient","id":"{id}","active":true}}"#);
    let reply = request(app, "POST", "/Patient", body.as_bytes()).await;
    assert!(reply.status.is_success(), "{}", reply.body);
}

#[tokio::test]
async fn validate_reports_a_submitted_resource_without_storing_it() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-v1","active":true}"#;
    let reply = request(&app, "POST", "/Patient/$validate", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let value = json(&reply);
    assert_eq!(value["resourceType"], "OperationOutcome");
    assert_eq!(value["issue"][0]["severity"], "information");
    assert_eq!(request(&app, "GET", "/Patient/pt-v1", &[]).await.status, StatusCode::NOT_FOUND);
    assert_eq!(json(&request(&app, "GET", "/Patient", &[]).await)["total"], 0);
}

#[tokio::test]
async fn validate_reports_a_resource_that_contradicts_its_path() {
    let app = service();
    let body = br#"{"resourceType":"Observation","id":"ob-1"}"#;
    let reply = request(&app, "POST", "/Patient/$validate", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(json(&reply)["issue"][0]["severity"], "error");
}

#[tokio::test]
async fn validate_reads_a_stored_resource_and_leaves_its_version() {
    let app = service();
    create_patient(&app, "pt-v2").await;
    let reply = request(&app, "GET", "/Patient/pt-v2/$validate", &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(json(&reply)["issue"][0]["severity"], "information");
    let history = request(&app, "GET", "/Patient/pt-v2/_history", &[]).await;
    assert_eq!(json(&history)["total"], 1);
}

#[tokio::test]
async fn validate_checks_a_profile_and_a_narrative() {
    let app = service();
    let body = br#"{"resourceType":"Patient","id":"pt-v3","text":{"status":"invented","div":"plain"}}"#;
    let reply = request(&app, "POST", "/Patient/$validate?profile=http://x/one", body).await;
    assert_eq!(reply.status, StatusCode::OK);
    let issues = json(&reply)["issue"].as_array().cloned().unwrap_or_default();
    assert!(issues.len() >= 3, "{}", reply.body);
    assert!(issues.iter().all(|issue| issue["severity"] == "error"));
}

#[tokio::test]
async fn validate_takes_the_resource_from_an_input_parameters_body() {
    let app = service();
    let body = serde_json::to_vec(&serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "resource", "resource": {"resourceType": "Patient", "id": "pt-v4"}},
            {"name": "mode", "valueCode": "create"}
        ]
    }))
    .unwrap();
    let reply = request(&app, "POST", "/Patient/$validate", &body).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(json(&reply)["issue"][0]["severity"], "warning");
}

#[tokio::test]
async fn validate_refuses_a_malformed_body_and_an_unknown_mode() {
    let app = service();
    let malformed = request(&app, "POST", "/Patient/$validate", b"not json").await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    let empty = request(&app, "POST", "/Patient/$validate", &[]).await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    let body = br#"{"resourceType":"Patient","id":"pt-v5"}"#;
    let mode = request(&app, "POST", "/Patient/$validate?mode=nonesuch", body).await;
    assert_eq!(mode.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn validate_reports_an_unknown_stored_resource() {
    let app = service();
    let reply = request(&app, "GET", "/Patient/nonesuch/$validate", &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

fn stepping_service() -> Service {
    let step = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let clock = Arc::new(move || {
        let minute = step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FhirInstant::parse(&format!("2026-09-06T04:{minute:02}:00.000Z")).unwrap()
    });
    let store = MemoryStore::with_clock(clock);
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn create(app: &Service, resource: serde_json::Value) {
    let kind = resource["resourceType"].as_str().unwrap().to_owned();
    let body = serde_json::to_vec(&resource).unwrap();
    let reply = request(app, "POST", &format!("/{kind}"), &body).await;
    assert!(reply.status.is_success(), "{}", reply.body);
}

fn observation(id: &str, patient: &str) -> serde_json::Value {
    serde_json::json!({
        "resourceType": "Observation",
        "id": id,
        "status": "final",
        "subject": {"reference": format!("Patient/{patient}")}
    })
}

fn ids(value: &Value) -> Vec<String> {
    value["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["resource"]["id"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn everything_gathers_the_patient_and_its_compartment() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e1"})).await;
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e2"})).await;
    create(&app, observation("ob-e1", "pt-e1")).await;
    create(&app, observation("ob-e2", "pt-e2")).await;
    let reply = request(&app, "GET", "/Patient/pt-e1/$everything", &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    let value = json(&reply);
    assert_eq!(value["type"], "searchset");
    let gathered = ids(&value);
    assert!(gathered.contains(&"pt-e1".to_owned()), "{gathered:?}");
    assert!(gathered.contains(&"ob-e1".to_owned()), "{gathered:?}");
    assert!(!gathered.contains(&"ob-e2".to_owned()), "{gathered:?}");
    assert!(!gathered.contains(&"pt-e2".to_owned()), "{gathered:?}");
}

#[tokio::test]
async fn everything_answers_the_same_over_post() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e3"})).await;
    let got = request(&app, "GET", "/Patient/pt-e3/$everything", &[]).await;
    let posted = request(&app, "POST", "/Patient/pt-e3/$everything", &[]).await;
    assert_eq!(posted.status, StatusCode::OK);
    assert_eq!(ids(&json(&got)), ids(&json(&posted)));
}

#[tokio::test]
async fn everything_narrows_by_type_and_time() {
    let app = stepping_service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e4"})).await;
    create(&app, observation("ob-e4", "pt-e4")).await;
    let typed = request(&app, "GET", "/Patient/pt-e4/$everything?_type=Observation", &[]).await;
    assert_eq!(ids(&json(&typed)), vec!["ob-e4".to_owned()]);
    let since = request(
        &app,
        "GET",
        "/Patient/pt-e4/$everything?_since=2026-09-06T04:01:00Z",
        &[],
    )
    .await;
    assert_eq!(ids(&json(&since)), vec!["ob-e4".to_owned()]);
    let till = request(
        &app,
        "GET",
        "/Patient/pt-e4/$everything?_till=2026-09-06T04:00:30Z",
        &[],
    )
    .await;
    assert_eq!(ids(&json(&till)), vec!["pt-e4".to_owned()]);
}

#[tokio::test]
async fn everything_pages_through_search_continuation_tokens() {
    let app = service();
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e5"})).await;
    create(&app, observation("ob-e5", "pt-e5")).await;
    let first = request(&app, "GET", "/Patient/pt-e5/$everything?_count=1", &[]).await;
    let value = json(&first);
    assert_eq!(value["total"], 2);
    assert_eq!(ids(&value).len(), 1);
    let next = value["link"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["relation"] == "next")
        .map(|link| link["url"].as_str().unwrap().to_owned())
        .expect("a next link");
    assert!(next.contains("ct="), "{next}");
    let path = next.split_once("localhost").unwrap().1.to_owned();
    let second = request(&app, "GET", &path, &[]).await;
    assert_eq!(ids(&json(&second)).len(), 1);
    assert_ne!(ids(&json(&second)), ids(&value));
}

#[tokio::test]
async fn everything_refuses_an_unknown_patient_and_parameter() {
    let app = service();
    let missing = request(&app, "GET", "/Patient/nonesuch/$everything", &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    create(&app, serde_json::json!({"resourceType": "Patient", "id": "pt-e6"})).await;
    let unknown = request(&app, "GET", "/Patient/pt-e6/$everything?nonesuch=1", &[]).await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let typed = request(&app, "GET", "/Patient/pt-e6/$everything?_type=Medication", &[]).await;
    assert_eq!(typed.status, StatusCode::BAD_REQUEST);
}
