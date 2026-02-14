use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Authorization, Dependency, HeldKeys, Service};
use fhir_core::security::bearer::KeySet;
use fhir_core::security::fixture::Issuer;
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

const ISSUER: &str = "https://issuer.example.org";

fn signing() -> &'static Issuer {
    static HELD: OnceLock<Issuer> = OnceLock::new();
    HELD.get_or_init(|| Issuer::generate("one"))
}

struct Reply {
    status: StatusCode,
    body: String,
}

pub fn keys() -> KeySet {
    KeySet::parse(&signing().keys()).expect("a published key set")
}

fn guarded() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
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
    signing().mint(&payload)
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
            r#"{{"resource":{{"resourceType":"Patient","id":"{id}","active":true}},"request":{{"method":"POST","url":"Patient"}}}},{{"resource":{{"resourceType":"Observation","id":"ob-{id}","status":"final","code":{{"text":"probe"}}}},"request":{{"method":"POST","url":"Observation"}}}}"#
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
        br#"{"resourceType":"Observation","id":"ob-a","status":"final","code":{"text":"probe"},"subject":{"reference":"Patient/pt-a"}}"#.to_vec(),
        br#"{"resourceType":"Observation","id":"ob-b","status":"final","code":{"text":"probe"},"subject":{"reference":"Patient/pt-b"}}"#.to_vec(),
        br#"{"resourceType":"Observation","id":"ob-c","status":"amended","code":{"text":"probe"},"subject":{"reference":"Patient/pt-a"}}"#.to_vec(),
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
            check: Arc::new(|| Box::pin(async { Ok(()) })),
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

#[tokio::test]
async fn introspection_reports_a_token_to_a_caller_that_carries_one() {
    let app = guarded();
    let asked = token("system/Patient.read");
    let body = format!("token={asked}");
    let reported = with_token(
        &app,
        "POST",
        "/_introspect",
        &token("system/Patient.read"),
        body.as_bytes(),
    )
    .await;
    let anonymous = call(&app, "POST", "/_introspect", None, body.as_bytes()).await;
    let forged = with_token(
        &app,
        "POST",
        "/_introspect",
        &token("system/Patient.read"),
        b"token=not-a-token",
    )
    .await;

    assert_eq!(reported.status, StatusCode::OK, "{}", reported.body);
    let value: Value = serde_json::from_str(&reported.body).expect("a document");
    assert_eq!(value["active"], true);
    assert_eq!(value["sub"], "practitioner-1");
    assert_eq!(value["scope"], "system/Patient.read");
    assert_eq!(value["iss"], ISSUER);
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(forged.status, StatusCode::OK, "{}", forged.body);
    let inactive: Value = serde_json::from_str(&forged.body).expect("a document");
    assert_eq!(inactive["active"], false);
    assert!(inactive.get("sub").is_none());
}

#[tokio::test]
async fn an_unsecured_instance_introspects_nothing() {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let app = Service::new(
        Arc::new(store),
        FhirVersion::R4,
        vec![Dependency {
            name: "memory-store",
            check: Arc::new(|| Box::pin(async { Ok(()) })),
        }],
    );
    let reply = call(&app, "POST", "/_introspect", None, b"token=abc").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

fn recording() -> (Service, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })));
    let app = Service::new(
        Arc::clone(&store) as Arc<dyn fhir_store::ResourceStore>,
        FhirVersion::R4,
        vec![Dependency {
            name: "memory-store",
            check: Arc::new(|| Box::pin(async { Ok(()) })),
        }],
    )
    .with_authorization(Authorization::new(
        ISSUER,
        "https://issuer.example.org/a",
        "https://issuer.example.org/t",
    ))
    .enforcing(Arc::new(HeldKeys::new(keys())))
    .expect("an authorization is configured");
    let trail = Arc::new(fhir_api::StoredTrail::new(
        Arc::clone(&store) as Arc<dyn fhir_store::ResourceStore>,
        FhirVersion::R4,
    ));
    (app.recording(trail), store)
}

async fn trail_of(store: &Arc<MemoryStore>) -> Vec<Value> {
    use fhir_store::{ResourceStore, SearchQuery};
    let query = SearchQuery::of_type("AuditEvent".parse().unwrap());
    store
        .search(&query)
        .await
        .expect("the trail is searchable")
        .entries
        .iter()
        .map(|entry| serde_json::from_slice::<Value>(entry.raw()).expect("a record"))
        .collect()
}

#[tokio::test]
async fn every_read_and_write_leaves_one_record_of_who_did_what() {
    let (app, store) = recording();
    let scopes = Some("system/Patient.read system/Patient.write");
    let created = call(
        &app,
        "POST",
        "/Patient",
        scopes,
        br#"{"resourceType":"Patient","id":"pt-t1","active":true,"name":[{"family":"Stone"}]}"#,
    )
    .await;
    let read = call(&app, "GET", "/Patient/pt-t1", scopes, &[]).await;
    let refused = call(&app, "GET", "/Observation/ob-t1", scopes, &[]).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    let records = trail_of(&store).await;
    assert_eq!(records.len(), 3, "{records:?}");
    let actions: Vec<String> = records
        .iter()
        .map(|entry| entry["type"]["code"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(actions.contains(&"write".to_owned()), "{actions:?}");
    assert!(actions.contains(&"read".to_owned()), "{actions:?}");
    for entry in &records {
        assert_eq!(
            entry["agent"][0]["who"]["identifier"]["value"],
            "practitioner-1"
        );
        let text = entry.to_string();
        assert!(!text.contains("Stone"), "{text}");
        assert!(!text.contains("Bearer"), "{text}");
    }
    let denied = records
        .iter()
        .find(|entry| entry["outcome"] == "8")
        .expect("the refusal is recorded");
    assert_eq!(denied["entity"][0]["what"]["reference"], "Observation/ob-t1");
}

#[tokio::test]
async fn a_refused_credential_never_reaches_the_error_body() {
    let (app, _store) = recording();
    let secret = "a-token-that-does-not-verify";
    let request = Request::builder()
        .method("GET")
        .uri("/Patient/pt-t1")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {secret}"))
        .body(Body::empty())
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes).into_owned();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.contains(secret), "{body}");
}

fn granted() -> Value {
    json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": "system/*.read system/*.write",
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    })
}

fn refusable() -> Vec<(&'static str, String)> {
    let elsewhere = Issuer::generate("one");
    let symmetric = json!({"alg": "HS256", "kid": "one"});
    let input = Issuer::input(&symmetric, &granted());
    let forged = jsonwebtoken::crypto::sign(
        input.as_bytes(),
        &jsonwebtoken::EncodingKey::from_secret(signing().material().as_bytes()),
        jsonwebtoken::Algorithm::HS256,
    )
    .expect("the library signs");
    let mut stale = granted();
    stale["exp"] = json!(time::OffsetDateTime::now_utc().unix_timestamp() - 60);
    vec![
        ("signed by another key", elsewhere.mint(&granted())),
        (
            "carrying no signature",
            format!(
                "{}.",
                Issuer::input(&json!({"alg": "ES256", "kid": "one"}), &granted())
            ),
        ),
        ("naming a symmetric algorithm", format!("{input}.{forged}")),
        (
            "naming an algorithm the key does not verify",
            signing().minted_under(&json!({"alg": "ES384", "kid": "one"}), &granted()),
        ),
        ("expired", signing().mint(&stale)),
    ]
}

async fn held(store: &Arc<MemoryStore>, kind: &str) -> usize {
    use fhir_store::{ResourceStore, SearchQuery};
    let query = SearchQuery::of_type(kind.parse().unwrap());
    store
        .search(&query)
        .await
        .expect("the store is searchable")
        .entries
        .len()
}

#[tokio::test]
async fn a_token_the_published_key_does_not_verify_never_reaches_the_store() {
    let (app, store) = recording();
    for (reason, offered) in refusable() {
        let read = with_token(&app, "GET", "/Patient/pt-s1", &offered, &[]).await;
        let write = with_token(&app, "POST", "/Patient", &offered, PATIENT).await;
        assert_eq!(read.status, StatusCode::UNAUTHORIZED, "{reason}: {}", read.body);
        assert_eq!(code(&read.body), "login", "{reason}");
        assert_eq!(write.status, StatusCode::UNAUTHORIZED, "{reason}: {}", write.body);
        assert!(!read.body.contains(&offered), "{reason}");
    }
    assert_eq!(held(&store, "Patient").await, 0);
    assert!(trail_of(&store).await.is_empty());
}

#[tokio::test]
async fn an_issuer_offering_a_symmetric_key_configures_no_instance() {
    let shared = json!({"keys": [
        {"kty": "oct", "kid": "one", "alg": "HS256", "k": "c2hhcmVk"}
    ]});
    assert!(KeySet::parse(&shared).is_err());
    let mut mixed = signing().keys();
    mixed["keys"]
        .as_array_mut()
        .expect("a listed key")
        .push(shared["keys"][0].clone());
    assert!(KeySet::parse(&mixed).is_err());
}
