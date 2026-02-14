use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service, METRICS};
use fhir_core::{FhirInstant, FhirVersion};
use fhir_store::StepTicker;
use fhir_telemetry::{Held, Scrape, Telemetry};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

const CREDENTIAL: &str = "a-reader-credential";

struct Reply {
    status: StatusCode,
    kind: String,
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-07T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    let sink = Held::default();
    let ticker = StepTicker::starting_at(0).ticker();
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
        .reporting(Arc::new(Telemetry::new(sink.sink(), ticker)))
}

async fn call(app: &Service, uri: &str, credential: Option<&str>) -> Reply {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "localhost");
    if let Some(credential) = credential {
        builder = builder.header("authorization", format!("Bearer {credential}"));
    }
    let response = app
        .router()
        .oneshot(builder.body(Body::from(Vec::new())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|held| held.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        kind,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

#[tokio::test]
async fn an_unconfigured_instance_does_not_serve_its_measurements() {
    let app = service();
    for credential in [None, Some(CREDENTIAL)] {
        let reply = call(&app, METRICS, credential).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        assert!(!reply.body.contains("fhir_operation_total"));
        assert!(!reply.body.contains("duration_ms"));
    }
}

#[tokio::test]
async fn an_unconfigured_instance_answers_as_it_does_for_an_unrouted_path() {
    let app = service();
    let restricted = call(&app, METRICS, None).await;
    let absent = call(&app, "/one/two/three/four", None).await;
    assert_eq!(restricted.status, absent.status);
    assert_eq!(restricted.body, absent.body);
}

#[tokio::test]
async fn a_reader_without_the_credential_is_refused() {
    let app = service().scraped(Scrape::guarded(CREDENTIAL).unwrap());
    for credential in [None, Some("another-reader-credential")] {
        let reply = call(&app, METRICS, credential).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert!(!reply.body.contains("fhir_operation_total"));
    }
}

#[tokio::test]
async fn a_reader_with_the_credential_reads_the_measurements() {
    let app = service().scraped(Scrape::guarded(CREDENTIAL).unwrap());
    call(&app, "/metadata", None).await;
    let reply = call(&app, METRICS, Some(CREDENTIAL)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.kind.starts_with("text/plain"));
    assert!(reply
        .body
        .contains("fhir_operation_total{operation=\"conformance\",outcome=\"success\"} 1"));
    assert!(reply.body.contains("fhir_telemetry_suppressed_total"));
}

#[tokio::test]
async fn the_measurements_name_no_resource_a_caller_asked_for() {
    let app = service().scraped(Scrape::guarded(CREDENTIAL).unwrap());
    call(&app, "/Patient/pt-confidential-77", None).await;
    call(&app, "/Patient?family=Rossignol", None).await;
    let reply = call(&app, METRICS, Some(CREDENTIAL)).await;
    assert_eq!(reply.status, StatusCode::OK);
    for secret in ["pt-confidential-77", "Rossignol", CREDENTIAL, "family"] {
        assert!(!reply.body.contains(secret), "{secret} was served");
    }
}
