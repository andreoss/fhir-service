use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Authorization, HeldKeys, Introspection, Service};
use fhir_core::security::bearer::KeySet;
use fhir_core::security::fixture::Issuer;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

const ISSUER: &str = "https://issuer.example.org";

struct Stub {
    address: String,
    asked: Arc<std::sync::atomic::AtomicUsize>,
}

async fn stub(reply: &'static str) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = format!(
        "http://{}/introspect",
        listener.local_addr().expect("bound")
    );
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut raw = vec![0_u8; 4096];
            let read = socket.read(&mut raw).await.unwrap_or_default();
            let asked = String::from_utf8_lossy(&raw[..read]).to_string();
            assert!(asked.starts_with("POST /introspect"), "{asked}");
            assert!(asked.contains("authorization: Basic"), "{asked}");
            assert!(asked.contains("token=an-opaque-token"), "{asked}");
            let answer = format!(
                "HTTP/1.0 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                reply.len()
            );
            let _ = socket.write_all(answer.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    Stub { address, asked }
}

fn guarded(endpoint: &str) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let issuer = Issuer::generate("one");
    let keys = KeySet::parse(&issuer.keys()).expect("a published key set");
    let asking = Introspection::new(
        endpoint,
        "an-app",
        "a-secret",
        ISSUER,
        None,
        std::time::Duration::from_secs(2),
    )
    .expect("a loopback endpoint is allowed");
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .with_authorization(Authorization::new(
            ISSUER,
            "https://issuer.example.org/a",
            "https://issuer.example.org/t",
        ))
        .asking(Arc::new(HeldKeys::new(keys)), Arc::new(asking))
        .expect("an authorization is configured")
}

async fn read(app: &Service, token: &str) -> StatusCode {
    let request = Request::builder()
        .method("GET")
        .uri("/Patient/nobody")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let _ = response.into_body().collect().await;
    status
}

#[tokio::test]
async fn a_token_the_issuer_says_is_active_reaches_the_records_it_names() {
    let held = stub(r#"{"active":true,"sub":"practitioner-1","scope":"system/*.read"}"#).await;
    let app = guarded(&held.address);
    assert_eq!(read(&app, "an-opaque-token").await, StatusCode::NOT_FOUND);
    assert_eq!(held.asked.load(std::sync::atomic::Ordering::SeqCst), 1);

    assert_eq!(read(&app, "an-opaque-token").await, StatusCode::NOT_FOUND);
    assert_eq!(
        held.asked.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the same token is not asked about twice"
    );
}

#[tokio::test]
async fn a_token_the_issuer_says_is_not_active_reaches_nothing() {
    let held = stub(r#"{"active":false}"#).await;
    let app = guarded(&held.address);
    assert_eq!(
        read(&app, "an-opaque-token").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_token_the_issuer_cannot_be_asked_about_reaches_nothing() {
    let app = guarded("http://127.0.0.1:9/introspect");
    assert_eq!(
        read(&app, "an-opaque-token").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_signed_token_is_still_read_where_it_is_signed() {
    let held = stub(r#"{"active":true,"scope":"system/*.read"}"#).await;
    let app = guarded(&held.address);
    let forged = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJodHRwczovL2lzc3Vlci5leGFtcGxlLm9yZyJ9.bm90";
    assert_eq!(read(&app, forged).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        held.asked.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a token that carries a signature is judged by the signature"
    );
}
