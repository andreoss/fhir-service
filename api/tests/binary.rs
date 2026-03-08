use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Artifacts, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn service(artifacts: Artifacts) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).holding_artifacts(artifacts)
}

struct Reply {
    status: StatusCode,
    media_type: String,
    location: String,
    body: Vec<u8>,
}

async fn ask(
    app: &Service,
    method: &str,
    uri: &str,
    content_type: Option<&str>,
    accept: Option<&str>,
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if let Some(held) = content_type {
        builder = builder.header("content-type", held);
    }
    if let Some(held) = accept {
        builder = builder.header("accept", held);
    }
    let response = app
        .router()
        .oneshot(builder.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let header = |name: &str| -> String {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let media_type = header("content-type");
    let location = header("location");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        media_type,
        location,
        body: bytes.to_vec(),
    }
}

const PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\n%%EOF";

#[tokio::test]
async fn an_artifact_is_stored_and_handed_back_as_it_came() {
    let app = service(Artifacts::default());
    let written = ask(
        &app,
        "PUT",
        "/Binary/bn-1",
        Some("application/pdf"),
        None,
        PDF,
    )
    .await;
    assert_eq!(written.status, StatusCode::CREATED);
    assert!(
        written.location.contains("/Binary/bn-1"),
        "{}",
        written.location
    );

    let read = ask(
        &app,
        "GET",
        "/Binary/bn-1",
        None,
        Some("application/pdf"),
        b"",
    )
    .await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.media_type, "application/pdf");
    assert_eq!(read.body, PDF, "byte for byte what was sent");
}

#[tokio::test]
async fn the_fhir_resource_underneath_is_a_binary() {
    let app = service(Artifacts::default());
    ask(
        &app,
        "PUT",
        "/Binary/bn-2",
        Some("text/plain"),
        None,
        b"held",
    )
    .await;
    let read = ask(
        &app,
        "GET",
        "/Binary/bn-2",
        None,
        Some("application/fhir+json"),
        b"",
    )
    .await;
    assert_eq!(read.status, StatusCode::OK);
    let held: Value = serde_json::from_slice(&read.body).expect("a resource");
    assert_eq!(held["resourceType"], "Binary");
    assert_eq!(held["contentType"], "text/plain");
    assert_eq!(held["meta"]["versionId"], "1");
}

#[tokio::test]
async fn posting_an_artifact_gives_it_an_address() {
    let app = service(Artifacts::default());
    let written = ask(&app, "POST", "/Binary", Some("text/plain"), None, b"held").await;
    assert_eq!(written.status, StatusCode::CREATED);
    assert!(
        written.location.contains("/Binary/"),
        "{}",
        written.location
    );
}

#[tokio::test]
async fn a_media_type_the_instance_does_not_hold_is_refused() {
    let app = service(Artifacts::default());
    let written = ask(
        &app,
        "PUT",
        "/Binary/bn-3",
        Some("application/x-shockwave-flash"),
        None,
        b"held",
    )
    .await;
    assert_eq!(written.status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn an_artifact_past_the_size_the_instance_holds_is_refused() {
    let app = service(Artifacts {
        most_bytes: 8,
        ..Artifacts::default()
    });
    let written = ask(
        &app,
        "PUT",
        "/Binary/bn-4",
        Some("text/plain"),
        None,
        b"far too many bytes for this instance",
    )
    .await;
    assert_eq!(written.status, StatusCode::BAD_REQUEST);
    assert!(
        String::from_utf8_lossy(&written.body).contains("past the 8"),
        "{}",
        String::from_utf8_lossy(&written.body)
    );
}

#[tokio::test]
async fn writing_over_an_artifact_is_a_version() {
    let app = service(Artifacts::default());
    ask(
        &app,
        "PUT",
        "/Binary/bn-5",
        Some("text/plain"),
        None,
        b"one",
    )
    .await;
    let again = ask(
        &app,
        "PUT",
        "/Binary/bn-5",
        Some("text/plain"),
        None,
        b"two",
    )
    .await;
    assert_eq!(again.status, StatusCode::OK);
    let read = ask(&app, "GET", "/Binary/bn-5", None, Some("text/plain"), b"").await;
    assert_eq!(read.body, b"two");
    let held = ask(
        &app,
        "GET",
        "/Binary/bn-5",
        None,
        Some("application/fhir+json"),
        b"",
    )
    .await;
    let resource: Value = serde_json::from_slice(&held.body).unwrap();
    assert_eq!(resource["meta"]["versionId"], "2");
}

#[tokio::test]
async fn an_artifact_that_is_not_there_is_not_found() {
    let app = service(Artifacts::default());
    let read = ask(&app, "GET", "/Binary/nobody", None, Some("text/plain"), b"").await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
}
