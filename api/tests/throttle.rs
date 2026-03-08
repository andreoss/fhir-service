use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Service, Throttle};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    retry_after: String,
    body: String,
}

fn service(throttle: Throttle) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new()).with_throttle(throttle)
}

async fn ask(app: &Service, uri: &str) -> Reply {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        retry_after,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}


fn slow(app: &Service) -> tokio::task::JoinHandle<()> {
    let router = app.router();
    tokio::spawn(async move {
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "localhost")
            .header("content-type", "application/fhir+json")
            .body(Body::from(
                serde_json::json!({
                    "resourceType": "Bundle",
                    "type": "batch",
                    "entry": (0..64).map(|index| serde_json::json!({
                        "request": {"method": "GET", "url": format!("Patient/p{index}")}
                    })).collect::<Vec<_>>()
                })
                .to_string(),
            ))
            .unwrap();
        let _ = router.oneshot(request).await;
    })
}

#[tokio::test]
async fn nothing_configured_refuses_nothing() {
    let app = service(Throttle::unbounded());
    for _ in 0..8 {
        let reply = ask(&app, "/Patient/absent").await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    }
}

#[tokio::test]
async fn a_request_past_the_bound_is_refused_with_a_wait() {
    let app = service(Throttle::of(1).unwrap());
    
    let first = slow(&app);
    let mut refused = None;
    for _ in 0..200 {
        let reply = ask(&app, "/Patient/absent").await;
        if reply.status == StatusCode::TOO_MANY_REQUESTS {
            refused = Some(reply);
            break;
        }
        tokio::task::yield_now().await;
    }
    let _ = first.await;
    let refused = refused.expect("a request past the bound is refused");
    assert_eq!(refused.retry_after, "1", "{}", refused.body);
    assert!(refused.body.contains("throttled"), "{}", refused.body);
}

#[tokio::test]
async fn the_routes_that_report_on_the_instance_are_never_refused() {
    let app = service(Throttle::of(1).unwrap());
    let held = slow(&app);
    let mut refused_once = false;
    for _ in 0..200 {
        if ask(&app, "/Patient/absent").await.status == StatusCode::TOO_MANY_REQUESTS {
            refused_once = true;
            for uri in ["/health", "/metadata"] {
                let reply = ask(&app, uri).await;
                assert_eq!(
                    reply.status,
                    StatusCode::OK,
                    "{uri} is answered while the instance is refusing: {}",
                    reply.body
                );
            }
            break;
        }
        tokio::task::yield_now().await;
    }
    let _ = held.await;
    assert!(refused_once, "the instance reached its bound");
}

#[tokio::test]
async fn a_bound_instance_still_answers_below_the_bound() {
    let app = service(Throttle::of(4).unwrap());
    for _ in 0..16 {
        let reply = ask(&app, "/Patient/absent").await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    }
}

#[tokio::test]
async fn a_refusal_is_counted_as_throttled_rather_than_as_a_client_fault() {
    assert_eq!(
        fhir_telemetry::Outcome::of_status(429),
        fhir_telemetry::Outcome::Throttled
    );
    assert_eq!(
        fhir_telemetry::Outcome::of_status(400),
        fhir_telemetry::Outcome::ClientFault
    );
}
