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

async fn ask(app: &Service, uri: &str, body: &[u8]) -> (StatusCode, String) {
    let request = Request::builder()
        .method(if body.is_empty() { "GET" } else { "PUT" })
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

async fn observation(app: &Service, id: &str, code: &str, effective: &str, category: &str) {
    let body = json!({
        "resourceType": "Observation",
        "id": id,
        "status": "final",
        "category": [{"coding": [{"system": "urn:c", "code": category}]}],
        "code": {"coding": [{"system": "urn:s", "code": code}]},
        "effectiveDateTime": effective,
        "subject": {"reference": "Patient/ln-1"}
    })
    .to_string()
    .into_bytes();
    let (status, told) = ask(app, &format!("/Observation/{id}"), &body).await;
    assert!(status.is_success(), "{told}");
}

async fn seeded() -> Service {
    let app = service();
    let (status, told) = ask(
        &app,
        "/Patient/ln-1",
        json!({"resourceType": "Patient", "id": "ln-1"})
            .to_string()
            .as_bytes(),
    )
    .await;
    assert!(status.is_success(), "{told}");
    observation(&app, "bp-1", "bp", "2020-01-01", "vital-signs").await;
    observation(&app, "bp-2", "bp", "2022-01-01", "vital-signs").await;
    observation(&app, "bp-3", "bp", "2024-01-01", "vital-signs").await;
    observation(&app, "hr-1", "hr", "2021-01-01", "vital-signs").await;
    observation(&app, "hr-2", "hr", "2023-01-01", "vital-signs").await;
    observation(&app, "lab-1", "gluc", "2024-06-01", "laboratory").await;
    app
}

fn ids(body: &str) -> Vec<String> {
    let bundle: Value = serde_json::from_str(body).expect("a bundle");
    let mut held: Vec<String> = bundle["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["resource"]["id"].as_str().map(str::to_owned))
        .collect();
    held.sort();
    held
}

#[tokio::test]
async fn the_latest_of_each_code_is_answered() {
    let app = seeded().await;
    let (status, body) = ask(
        &app,
        "/Observation/$lastn?patient=ln-1&category=vital-signs",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec!["bp-3".to_owned(), "hr-2".to_owned()],
        "{body}"
    );
}

#[tokio::test]
async fn max_bounds_each_group_rather_than_the_answer() {
    let app = seeded().await;
    let (status, body) = ask(
        &app,
        "/Observation/$lastn?patient=ln-1&category=vital-signs&max=2",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![
            "bp-2".to_owned(),
            "bp-3".to_owned(),
            "hr-1".to_owned(),
            "hr-2".to_owned()
        ],
        "{body}"
    );
}

#[tokio::test]
async fn the_category_narrows_what_is_grouped() {
    let app = seeded().await;
    let (status, body) = ask(
        &app,
        "/Observation/$lastn?patient=ln-1&category=laboratory",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["lab-1".to_owned()], "{body}");
}

#[tokio::test]
async fn another_search_parameter_narrows_it_too() {
    let app = seeded().await;
    let (status, body) = ask(
        &app,
        "/Observation/$lastn?patient=ln-1&category=vital-signs&code=urn:s|bp",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec!["bp-3".to_owned()], "{body}");
}

#[tokio::test]
async fn a_subject_may_be_named_instead_of_a_patient() {
    let app = seeded().await;
    let (status, body) = ask(
        &app,
        "/Observation/$lastn?subject=Patient/ln-1&category=vital-signs",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body).len(), 2, "{body}");
}

#[tokio::test]
async fn naming_no_patient_or_no_category_is_refused() {
    let app = seeded().await;
    for uri in [
        "/Observation/$lastn?category=vital-signs",
        "/Observation/$lastn?patient=ln-1",
    ] {
        let (status, body) = ask(&app, uri, b"").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
    }
}

#[tokio::test]
async fn the_parameters_the_operation_orders_for_itself_are_refused() {
    let app = seeded().await;
    for refused in ["_sort=-date", "_count=1"] {
        let (status, body) = ask(
            &app,
            &format!("/Observation/$lastn?patient=ln-1&category=vital-signs&{refused}"),
            b"",
        )
        .await;
        assert!(status.is_client_error(), "{refused}: {status} {body}");
    }
}

#[tokio::test]
async fn a_max_that_is_not_a_count_is_refused() {
    let app = seeded().await;
    for raw in ["0", "many", "-1"] {
        let (status, body) = ask(
            &app,
            &format!("/Observation/$lastn?patient=ln-1&category=vital-signs&max={raw}"),
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}: {body}");
    }
}
