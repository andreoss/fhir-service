use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Authorization, Dependency, HeldKeys, Service};
use fhir_core::security::bearer::{encode, KeySet};
use fhir_core::security::digest::hmac_sha256;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &[u8] = b"a-secret-held-outside-the-source";
const ISSUER: &str = "https://issuer.example.org";

struct Reply {
    status: StatusCode,
    body: String,
}

pub fn keys() -> KeySet {
    KeySet::parse(&json!({"keys": [
        {"kty": "oct", "kid": "one", "alg": "HS256", "k": encode(SECRET)}
    ]}))
    .expect("a configured key set")
}

fn guarded() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Ok(())),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
        .with_authorization(Authorization::new(
            ISSUER,
            "https://issuer.example.org/a",
            "https://issuer.example.org/t",
        ))
        .enforcing(Arc::new(HeldKeys::new(keys())))
        .expect("an authorization is configured")
}

pub fn token(scopes: &str) -> String {
    minted(json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": scopes,
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }))
}

pub fn minted(payload: Value) -> String {
    let head = encode(&serde_json::to_vec(&json!({"alg": "HS256", "kid": "one"})).unwrap());
    let body = encode(&serde_json::to_vec(&payload).unwrap());
    let input = format!("{head}.{body}");
    format!("{input}.{}", encode(&hmac_sha256(SECRET, input.as_bytes())))
}

async fn call(app: &Service, method: &str, uri: &str, scopes: Option<&str>, body: &[u8]) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if let Some(scopes) = scopes {
        builder = builder.header("authorization", format!("Bearer {}", token(scopes)));
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn code(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["issue"][0]["code"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

const PATIENT: &[u8] = br#"{"resourceType":"Patient","id":"pt-s1","active":true}"#;

#[tokio::test]
async fn a_request_without_a_token_is_refused_before_the_store_is_touched() {
    let app = guarded();
    let read = call(&app, "GET", "/Patient/pt-s1", None, &[]).await;
    let write = call(&app, "POST", "/Patient", None, PATIENT).await;
    assert_eq!(read.status, StatusCode::UNAUTHORIZED);
    assert_eq!(code(&read.body), "login");
    assert_eq!(write.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn conformance_is_served_without_a_token() {
    let app = guarded();
    for path in ["/metadata", "/health", "/.well-known/smart-configuration"] {
        let reply = call(&app, "GET", path, None, &[]).await;
        assert_eq!(reply.status, StatusCode::OK, "{path} answered {}", reply.status);
    }
}

#[tokio::test]
async fn a_read_scope_reads_and_does_not_write() {
    let app = guarded();
    let created = call(&app, "POST", "/Patient", Some("system/Patient.write"), PATIENT).await;
    let read = call(&app, "GET", "/Patient/pt-s1", Some("system/Patient.read"), &[]).await;
    let refused = call(&app, "POST", "/Patient", Some("system/Patient.read"), PATIENT).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(code(&refused.body), "forbidden");
}

#[tokio::test]
async fn a_scope_over_one_type_does_not_reach_another() {
    let app = guarded();
    call(&app, "POST", "/Patient", Some("system/Patient.write"), PATIENT).await;
    let other = call(&app, "GET", "/Observation", Some("system/Patient.read"), &[]).await;
    let same = call(&app, "GET", "/Patient", Some("system/Patient.read"), &[]).await;
    assert_eq!(other.status, StatusCode::FORBIDDEN, "{}", other.body);
    assert_eq!(same.status, StatusCode::OK, "{}", same.body);
}

#[tokio::test]
async fn every_data_action_is_named_before_it_runs() {
    let app = guarded();
    let all = "system/*.read system/*.write";
    for (method, path, needed) in [
        ("POST", "/$export", "system/*.export"),
        ("POST", "/$import", "system/*.import"),
        ("POST", "/$reindex", "system/*.reindex"),
        ("POST", "/$bulk-delete", "system/*.bulk-delete"),
        ("POST", "/$bulk-update", "system/*.bulk-update"),
        ("POST", "/SearchParameter/$reindex", "system/*.parameter-management"),
    ] {
        let refused = call(&app, method, path, Some(all), b"{}").await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{path} with {all}");
        let granted = call(&app, method, path, Some(needed), b"{}").await;
        assert_ne!(granted.status, StatusCode::FORBIDDEN, "{path} with {needed}");
        assert_ne!(granted.status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn a_bundle_entry_outside_the_scope_fails_the_entry_or_the_bundle() {
    let app = guarded();
    let listed = |id: &str| {
        format!(
            r#"{{"resource":{{"resourceType":"Patient","id":"{id}","active":true}},"request":{{"method":"POST","url":"Patient"}}}},{{"resource":{{"resourceType":"Observation","id":"ob-{id}","status":"final"}},"request":{{"method":"POST","url":"Observation"}}}}"#
        )
    };
    let batch = format!(
        r#"{{"resourceType":"Bundle","type":"batch","entry":[{}]}}"#,
        listed("pt-s2")
    );
    let transaction = format!(
        r#"{{"resourceType":"Bundle","type":"transaction","entry":[{}]}}"#,
        listed("pt-s3")
    );
    let scope = Some("system/Patient.read system/Patient.write");
    let batched = call(&app, "POST", "/", scope, batch.as_bytes()).await;
    let atomic = call(&app, "POST", "/", scope, transaction.as_bytes()).await;
    let survived = call(&app, "GET", "/Patient/pt-s2", scope, &[]).await;
    let rolled_back = call(&app, "GET", "/Patient/pt-s3", scope, &[]).await;

    assert_eq!(batched.status, StatusCode::OK, "{}", batched.body);
    let replied: Value = serde_json::from_str(&batched.body).expect("a bundle");
    assert_eq!(replied["entry"][0]["response"]["status"], "201 Created");
    assert_eq!(replied["entry"][1]["response"]["status"], "403 Forbidden");
    assert_eq!(atomic.status, StatusCode::FORBIDDEN, "{}", atomic.body);
    assert_eq!(survived.status, StatusCode::OK);
    assert_eq!(rolled_back.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_expired_token_reads_nothing() {
    let app = guarded();
    let stale = minted(json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": "system/*.read",
        "exp": 1_000,
    }));
    let request = Request::builder()
        .method("GET")
        .uri("/Patient")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {stale}"))
        .body(Body::empty())
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
