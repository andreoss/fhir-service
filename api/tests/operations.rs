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
