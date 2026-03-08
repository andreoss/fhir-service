use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Endpoints, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn one(version: FhirVersion) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), version, Vec::new())
}

fn endpoints() -> axum::Router {
    Endpoints::new(
        FhirVersion::R4,
        vec![
            (FhirVersion::R4, one(FhirVersion::R4)),
            (FhirVersion::R5, one(FhirVersion::R5)),
        ],
    )
    .expect("two releases and a default among them")
    .router()
}

async fn ask(
    router: &axum::Router,
    method: &str,
    uri: &str,
    accept: Option<&str>,
    body: &[u8],
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json");
    if let Some(accept) = accept {
        builder = builder.header("accept", accept);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn release_of(body: &str) -> String {
    let held: Value = serde_json::from_str(body).expect("a statement");
    held["fhirVersion"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_path_naming_a_release_reaches_it() {
    let router = endpoints();
    let (status, body) = ask(&router, "GET", "/R4/metadata", None, b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(release_of(&body), "4.0.1");

    let (status, body) = ask(&router, "GET", "/R5/metadata", None, b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(release_of(&body), "5.0.0");
}

#[tokio::test]
async fn a_media_type_naming_a_release_reaches_it_too() {
    let router = endpoints();
    let (status, body) = ask(
        &router,
        "GET",
        "/metadata",
        Some("application/fhir+json; fhirVersion=5.0"),
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(release_of(&body), "5.0.0", "{body}");
}

#[tokio::test]
async fn naming_no_release_reaches_the_default() {
    let router = endpoints();
    let (status, body) = ask(&router, "GET", "/metadata", None, b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(release_of(&body), "4.0.1");
}

#[tokio::test]
async fn the_path_and_the_media_type_agree() {
    let router = endpoints();
    let (_, by_path) = ask(&router, "GET", "/R5/metadata", None, b"").await;
    let (_, by_type) = ask(
        &router,
        "GET",
        "/metadata",
        Some("application/fhir+json; fhirVersion=5.0"),
        b"",
    )
    .await;
    assert_eq!(release_of(&by_path), release_of(&by_type));
}

#[tokio::test]
async fn a_release_the_instance_does_not_serve_is_refused() {
    let router = endpoints();
    let (status, body) = ask(&router, "GET", "/STU3/metadata", None, b"").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("does not serve"), "{body}");
    assert!(
        body.contains("R4"),
        "the refusal names what is served: {body}"
    );
}

#[tokio::test]
async fn a_resource_written_to_one_release_is_not_in_the_other() {
    let router = endpoints();
    let body = json!({"resourceType": "Patient", "id": "mv-1", "active": true})
        .to_string()
        .into_bytes();
    let (status, told) = ask(&router, "PUT", "/R4/Patient/mv-1", None, &body).await;
    assert_eq!(status, StatusCode::CREATED, "{told}");

    let (status, held) = ask(&router, "GET", "/R4/Patient/mv-1", None, b"").await;
    assert_eq!(status, StatusCode::OK, "{held}");

    let (status, _) = ask(&router, "GET", "/R5/Patient/mv-1", None, b"").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a resource of one release is not a resource of another"
    );
}

#[tokio::test]
async fn the_versions_report_names_what_is_served_and_the_default() {
    let router = endpoints();
    let (status, body) = ask(&router, "GET", "/$versions", None, b"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held: Value = serde_json::from_str(&body).unwrap();
    let named: Vec<String> = held["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["name"] == "version")
        .filter_map(|entry| entry["valueString"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        named,
        vec!["4.0.1".to_owned(), "5.0.0".to_owned()],
        "{body}"
    );
    let default = held["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "default")
        .unwrap()["valueString"]
        .as_str()
        .unwrap();
    assert_eq!(default, "4.0.1");
}

#[tokio::test]
async fn a_default_not_among_the_releases_served_is_refused() {
    let held = Endpoints::new(
        FhirVersion::Stu3,
        vec![(FhirVersion::R4, one(FhirVersion::R4))],
    );
    assert!(held.is_err());
}

#[tokio::test]
async fn serving_no_release_and_serving_one_twice_are_both_refused() {
    assert!(Endpoints::new(FhirVersion::R4, Vec::new()).is_err());
    let twice = Endpoints::new(
        FhirVersion::R4,
        vec![
            (FhirVersion::R4, one(FhirVersion::R4)),
            (FhirVersion::R4, one(FhirVersion::R4)),
        ],
    );
    assert!(twice.is_err());
}
