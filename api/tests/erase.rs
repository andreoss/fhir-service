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

async fn put(app: &Service, uri: &str, body: Value) {
    let (status, told) = ask(app, "PUT", uri, body.to_string().as_bytes()).await;
    assert!(status.is_success(), "{uri}: {told}");
}

async fn seeded() -> Service {
    let app = service();
    for name in ["One", "Two", "Three"] {
        put(
            &app,
            "/Patient/er-1",
            json!({"resourceType": "Patient", "id": "er-1", "name": [{"family": name}]}),
        )
        .await;
    }
    app
}

#[tokio::test]
async fn erasing_an_instance_takes_every_version() {
    let app = seeded().await;
    let (status, body) = ask(&app, "POST", "/Patient/er-1/$erase", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = ask(&app, "GET", "/Patient/er-1", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = ask(&app, "GET", "/Patient/er-1/_history/1", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn erasing_a_version_takes_it_and_the_ones_before_it() {
    let app = seeded().await;
    let (status, body) = ask(&app, "POST", "/Patient/er-1/_history/2/$erase", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let told: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(told["parameter"][0]["valueInteger"], 2, "{body}");
    for version in ["1", "2"] {
        let (status, _) = ask(
            &app,
            "GET",
            &format!("/Patient/er-1/_history/{version}"),
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "version {version}");
    }
    let (status, held) = ask(&app, "GET", "/Patient/er-1", b"").await;
    assert_eq!(status, StatusCode::OK, "the current version is untouched");
    let current: Value = serde_json::from_str(&held).unwrap();
    assert_eq!(current["meta"]["versionId"], "3");
}

#[tokio::test]
async fn a_version_that_is_not_there_is_not_found() {
    let app = seeded().await;
    let (status, _) = ask(&app, "POST", "/Patient/er-1/_history/9/$erase", b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_audit_event_is_never_erased() {
    let app = service();
    put(
        &app,
        "/AuditEvent/ae-1",
        json!({
            "resourceType": "AuditEvent",
            "id": "ae-1",
            "type": {"code": "rest"},
            "recorded": "2026-01-01T00:00:00Z",
            "agent": [{"requestor": true}],
            "source": {"observer": {"display": "this instance"}}
        }),
    )
    .await;
    let (status, body) = ask(&app, "POST", "/AuditEvent/ae-1/$erase", b"").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("not a trail"), "{body}");
    let (status, _) = ask(&app, "GET", "/AuditEvent/ae-1", b"").await;
    assert_eq!(status, StatusCode::OK, "it is still there");
}

#[tokio::test]
async fn purging_a_compartment_takes_what_belongs_to_the_patient() {
    let app = service();
    put(
        &app,
        "/Patient/pg-1",
        json!({"resourceType": "Patient", "id": "pg-1"}),
    )
    .await;
    put(
        &app,
        "/Patient/pg-2",
        json!({"resourceType": "Patient", "id": "pg-2"}),
    )
    .await;
    for (id, subject) in [("ob-1", "pg-1"), ("ob-2", "pg-2")] {
        put(
            &app,
            &format!("/Observation/{id}"),
            json!({
                "resourceType": "Observation",
                "id": id,
                "status": "final",
                "code": {"text": "probe"},
                "subject": {"reference": format!("Patient/{subject}")}
            }),
        )
        .await;
    }
    let (status, body) = ask(&app, "POST", "/Patient/pg-1/$purge", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for gone in ["/Patient/pg-1", "/Observation/ob-1"] {
        let (status, _) = ask(&app, "GET", gone, b"").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{gone}");
    }
    for held in ["/Patient/pg-2", "/Observation/ob-2"] {
        let (status, _) = ask(&app, "GET", held, b"").await;
        assert_eq!(status, StatusCode::OK, "{held} belongs to another patient");
    }
}

#[tokio::test]
async fn a_purge_leaves_the_trail_of_what_it_did() {
    let app = service();
    put(
        &app,
        "/Patient/pg-3",
        json!({"resourceType": "Patient", "id": "pg-3"}),
    )
    .await;
    put(
        &app,
        "/Provenance/pv-1",
        json!({
            "resourceType": "Provenance",
            "id": "pv-1",
            "target": [{"reference": "Patient/pg-3"}],
            "recorded": "2026-01-01T00:00:00Z",
            "agent": [{"who": {"reference": "Patient/pg-3"}}]
        }),
    )
    .await;
    let (status, body) = ask(&app, "POST", "/Patient/pg-3/$purge", b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = ask(&app, "GET", "/Provenance/pv-1", b"").await;
    assert_eq!(status, StatusCode::OK, "the provenance is kept");
}

#[tokio::test]
async fn a_purge_names_what_it_removed() {
    let app = service();
    put(
        &app,
        "/Patient/pg-4",
        json!({"resourceType": "Patient", "id": "pg-4"}),
    )
    .await;
    let (_, body) = ask(&app, "POST", "/Patient/pg-4/$purge", b"").await;
    let told: Value = serde_json::from_str(&body).unwrap();
    assert!(
        told["parameter"][1]["valueString"]
            .as_str()
            .unwrap()
            .contains("Patient/pg-4"),
        "{body}"
    );
}
