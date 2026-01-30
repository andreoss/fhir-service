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

async fn with_token(app: &Service, method: &str, uri: &str, token: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn launched(scopes: &str, patient: &str) -> String {
    minted(json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": scopes,
        "patient": patient,
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }))
}

fn ids(body: &str) -> Vec<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["entry"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["resource"]["id"].as_str().map(str::to_owned))
        .collect()
}

async fn seeded(app: &Service) {
    let write = Some("system/*.write");
    for body in [
        br#"{"resourceType":"Patient","id":"pt-a","active":true}"#.to_vec(),
        br#"{"resourceType":"Patient","id":"pt-b","active":true}"#.to_vec(),
    ] {
        call(app, "POST", "/Patient", write, &body).await;
    }
    for body in [
        br#"{"resourceType":"Observation","id":"ob-a","status":"final","subject":{"reference":"Patient/pt-a"}}"#.to_vec(),
        br#"{"resourceType":"Observation","id":"ob-b","status":"final","subject":{"reference":"Patient/pt-b"}}"#.to_vec(),
        br#"{"resourceType":"Observation","id":"ob-c","status":"amended","subject":{"reference":"Patient/pt-a"}}"#.to_vec(),
    ] {
        call(app, "POST", "/Observation", write, &body).await;
    }
}

#[tokio::test]
async fn a_launch_compartment_confines_the_search_the_read_and_the_include() {
    let app = guarded();
    seeded(&app).await;
    let confined = launched("patient/Observation.rs patient/Patient.rs", "pt-a");
    let searched = with_token(&app, "GET", "/Observation", &confined, &[]).await;
    let mine = with_token(&app, "GET", "/Observation/ob-a", &confined, &[]).await;
    let theirs = with_token(&app, "GET", "/Observation/ob-b", &confined, &[]).await;
    let included = with_token(
        &app,
        "GET",
        "/Observation?_include=Observation:subject",
        &confined,
        &[],
    )
    .await;

    let found = ids(&searched.body);
    assert!(found.contains(&"ob-a".to_owned()), "{found:?}");
    assert!(!found.contains(&"ob-b".to_owned()), "{found:?}");
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert_eq!(theirs.status, StatusCode::NOT_FOUND, "{}", theirs.body);
    let pulled = ids(&included.body);
    assert!(pulled.contains(&"pt-a".to_owned()), "{pulled:?}");
    assert!(!pulled.contains(&"pt-b".to_owned()), "{pulled:?}");
}

#[tokio::test]
async fn a_search_parameter_grant_narrows_the_type_it_names() {
    let app = guarded();
    seeded(&app).await;
    let narrowed = token("system/Observation.rs?status=final");
    let searched = with_token(&app, "GET", "/Observation", &narrowed, &[]).await;
    let outside = with_token(&app, "GET", "/Observation/ob-c", &narrowed, &[]).await;
    let inside = with_token(&app, "GET", "/Observation/ob-a", &narrowed, &[]).await;

    let found = ids(&searched.body);
    assert!(found.contains(&"ob-a".to_owned()), "{found:?}");
    assert!(!found.contains(&"ob-c".to_owned()), "{found:?}");
    assert_eq!(outside.status, StatusCode::NOT_FOUND, "{}", outside.body);
    assert_eq!(inside.status, StatusCode::OK, "{}", inside.body);
}

#[tokio::test]
async fn history_is_never_wider_than_the_grant() {
    let app = guarded();
    seeded(&app).await;
    let confined = launched("patient/Observation.rs", "pt-a");
    let system = with_token(&app, "GET", "/_history", &confined, &[]).await;
    let typed = with_token(&app, "GET", "/Observation/_history", &confined, &[]).await;
    let theirs = with_token(&app, "GET", "/Observation/ob-b/_history", &confined, &[]).await;
    let mine = with_token(&app, "GET", "/Observation/ob-a/_history", &confined, &[]).await;
    let open = call(&app, "GET", "/Observation/_history", Some("system/*.read"), &[]).await;

    assert_eq!(system.status, StatusCode::FORBIDDEN, "{}", system.body);
    assert_eq!(typed.status, StatusCode::FORBIDDEN, "{}", typed.body);
    assert_eq!(theirs.status, StatusCode::NOT_FOUND, "{}", theirs.body);
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert_eq!(open.status, StatusCode::OK, "{}", open.body);
}

#[tokio::test]
async fn a_conditional_write_selects_only_inside_the_grant() {
    let app = guarded();
    seeded(&app).await;
    let confined = launched("patient/Observation.cruds", "pt-a");
    let refused = with_token(
        &app,
        "DELETE",
        "/Observation?_id=ob-b",
        &confined,
        &[],
    )
    .await;
    let survived = call(&app, "GET", "/Observation/ob-b", Some("system/*.read"), &[]).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.body);
    assert_eq!(survived.status, StatusCode::OK, "{}", survived.body);
}

fn location(reply: &Reply) -> String {
    reply.body.clone()
}

#[tokio::test]
async fn a_job_answers_only_the_caller_that_submitted_it() {
    use fhir_adapter_memory::{MemoryBulkStore, MemoryJobStore};
    use fhir_store::{BulkStore, JobId, JobStore, Output, StepTicker};

    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let ticker = StepTicker::starting_at(1_000);
    let jobs = Arc::new(MemoryJobStore::new(ticker.ticker()));
    let sink = Arc::new(MemoryBulkStore::new());
    let app = Service::new(
        Arc::new(store),
        FhirVersion::R4,
        vec![Dependency {
            name: "memory-store",
            check: Arc::new(|| Ok(())),
        }],
    )
    .with_jobs(Arc::clone(&jobs) as Arc<dyn JobStore>)
    .with_outputs(Arc::clone(&sink) as Arc<dyn BulkStore>)
    .with_authorization(Authorization::new(
        ISSUER,
        "https://issuer.example.org/a",
        "https://issuer.example.org/t",
    ))
    .enforcing(Arc::new(HeldKeys::new(keys())))
    .expect("an authorization is configured");

    let mine = minted(json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": "system/*.export system/*.read",
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }));
    let theirs = minted(json!({
        "iss": ISSUER,
        "sub": "practitioner-2",
        "scope": "system/*.export system/*.read",
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }));
    let submitted = with_token(&app, "POST", "/$export", &mine, b"{}").await;
    assert_eq!(submitted.status, StatusCode::ACCEPTED, "{}", location(&submitted));

    let listed = jobs.list(&Default::default()).await.expect("the queue lists");
    let record = listed.first().expect("one job was submitted").clone();
    assert_eq!(record.owner.as_deref(), Some("practitioner-1"));
    let id = record.id.as_str().to_owned();
    sink.write(
        &JobId::parse(&id).unwrap(),
        &Output::new("part-1.ndjson", "Patient", 1),
        b"{}\n",
    )
    .await
    .expect("the sink takes the file");

    let path = format!("/_jobs/{id}");
    let polled_by_owner = with_token(&app, "GET", &path, &mine, &[]).await;
    let polled_by_other = with_token(&app, "GET", &path, &theirs, &[]).await;
    let file = format!("/_jobs/{id}/part-1.ndjson");
    let read_by_owner = with_token(&app, "GET", &file, &mine, &[]).await;
    let read_by_other = with_token(&app, "GET", &file, &theirs, &[]).await;
    let cancelled_by_other = with_token(&app, "DELETE", &path, &theirs, &[]).await;

    assert_ne!(polled_by_owner.status, StatusCode::NOT_FOUND);
    assert_eq!(polled_by_other.status, StatusCode::NOT_FOUND);
    assert_ne!(read_by_owner.status, StatusCode::NOT_FOUND, "{}", read_by_owner.body);
    assert_eq!(read_by_other.status, StatusCode::NOT_FOUND);
    assert_eq!(cancelled_by_other.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_confined_grant_submits_no_bulk_job() {
    let app = guarded();
    let confined = launched("patient/*.export", "pt-a");
    let refused = with_token(&app, "POST", "/$export", &confined, b"{}").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
}
