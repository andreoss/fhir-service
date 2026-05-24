use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::Service;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
}

async fn ask(app: &Service, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, String) {
    let held = match &body {
        Some(value) => Body::from(serde_json::to_vec(value).expect("a body")),
        None => Body::empty(),
    };
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .body(held)
        .expect("a request");
    let response = app.router().oneshot(request).await.expect("an answer");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn holding() -> Service {
    let app = service();
    let records = [
        json!({"resourceType": "Patient", "id": "p1", "name": [{"family": "Stone"}]}),
        json!({
            "resourceType": "Condition",
            "id": "c1",
            "subject": {"reference": "Patient/p1"},
            "clinicalStatus": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-clinical", "code": "active"}]},
            "onsetDateTime": "2024-03-04"
        }),
        json!({
            "resourceType": "Procedure",
            "id": "pr1",
            "status": "completed",
            "subject": {"reference": "Patient/p1"}
        }),
        json!({
            "resourceType": "MedicationRequest",
            "id": "m1",
            "status": "active",
            "intent": "order",
            "subject": {"reference": "Patient/p1"},
            "medicationReference": {"reference": "Medication/med1"}
        }),
        json!({
            "resourceType": "DiagnosticReport",
            "id": "d1",
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "58410-2"}]},
            "subject": {"reference": "Patient/p1"}
        }),
    ];
    for record in records {
        let kind = record["resourceType"].as_str().expect("a type").to_owned();
        let id = record["id"].as_str().expect("an id").to_owned();
        let (status, body) = ask(&app, "PUT", &format!("/{kind}/{id}"), Some(record)).await;
        assert_eq!(status, StatusCode::CREATED, "{kind}/{id}: {body}");
    }
    app
}

fn total(body: &str) -> i64 {
    let held: Value = serde_json::from_str(body).expect("a bundle");
    held["total"].as_i64().unwrap_or_else(|| {
        held["entry"]
            .as_array()
            .map(|rows| rows.len() as i64)
            .unwrap_or(0)
    })
}

#[tokio::test]
async fn the_patient_alias_answers_for_every_clinical_type_a_client_asks_it_of() {
    let app = holding().await;
    for uri in [
        "/Condition?patient=Patient/p1&_summary=count",
        "/Procedure?patient=Patient/p1&_summary=count",
        "/MedicationRequest?patient=Patient/p1&_summary=count",
        "/DiagnosticReport?patient=Patient/p1&_summary=count",
        "/Observation?patient=Patient/p1&_summary=count",
        "/Condition?patient=p1&_summary=count",
    ] {
        let (status, body) = ask(&app, "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        if !uri.starts_with("/Observation") {
            assert_eq!(total(&body), 1, "{uri}: {body}");
        }
    }
}

#[tokio::test]
async fn the_clinical_date_and_the_medication_named_are_searchable() {
    let app = holding().await;
    for (uri, expected) in [
        ("/Condition?onset-date=2024-03-04&_summary=count", 1),
        ("/Condition?onset-date=ge2024-01-01&_summary=count", 1),
        ("/Condition?onset-date=lt2023-01-01&_summary=count", 0),
        (
            "/MedicationRequest?medication=Medication/med1&_summary=count",
            1,
        ),
    ] {
        let (status, body) = ask(&app, "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(total(&body), expected, "{uri}: {body}");
    }
}

#[tokio::test]
async fn the_statement_declares_the_aliases_it_answers() {
    let app = service();
    let (status, body) = ask(&app, "GET", "/metadata", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).expect("a statement");
    let resources = held["rest"][0]["resource"].as_array().expect("types");
    for (kind, name) in [
        ("Condition", "patient"),
        ("Condition", "onset-date"),
        ("Procedure", "patient"),
        ("MedicationRequest", "patient"),
        ("MedicationRequest", "medication"),
        ("DiagnosticReport", "patient"),
    ] {
        let entry = resources
            .iter()
            .find(|entry| entry["type"] == kind)
            .unwrap_or_else(|| panic!("{kind} is served"));
        let named = entry["searchParam"]
            .as_array()
            .map(|params| params.iter().any(|param| param["name"] == name))
            .unwrap_or(false);
        assert!(named, "{kind}.{name} is declared");
    }
}
