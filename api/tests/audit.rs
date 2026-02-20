use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Authorization, Dependency, HeldKeys, Service, StoredTrail};
use fhir_core::security::bearer::KeySet;
use fhir_core::security::fixture::Issuer;
use fhir_core::{FhirInstant, FhirVersion, ResourceEnvelope, ResourceId};
use fhir_store::{ResourceStore, Seal, SearchQuery};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

const ISSUER: &str = "https://issuer.example.org";
const KEY: &str = "a-key-the-store-never-sees";
const EVERYTHING: &str = "system/*.cruds system/*.read system/*.write system/*.export system/*.bulk-delete";

fn signing() -> &'static Issuer {
    static HELD: OnceLock<Issuer> = OnceLock::new();
    HELD.get_or_init(|| Issuer::generate("trail"))
}

fn keys() -> KeySet {
    KeySet::parse(&signing().keys()).expect("a published key set")
}

fn token() -> String {
    signing().mint(&json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": EVERYTHING,
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }))
}

struct Reply {
    status: StatusCode,
    body: String,
}

fn recording() -> (Service, Arc<MemoryStore>, Arc<StoredTrail>) {
    let store = Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })));
    let held = Arc::clone(&store) as Arc<dyn ResourceStore>;
    let app = Service::new(
        Arc::clone(&held),
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
    let trail = Arc::new(StoredTrail::new(held, FhirVersion::R4).sealed_with(Seal::keyed(KEY)));
    (app.recording(Arc::clone(&trail)), store, trail)
}

async fn call(app: &Service, method: &str, uri: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("authorization", format!("Bearer {}", token()))
        .body(Body::from(body.to_vec()))
        .expect("a request");
    let response = app.router().oneshot(request).await.expect("a response");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn worked(app: &Service) {
    let created = call(
        app,
        "POST",
        "/Patient",
        br#"{"resourceType":"Patient","id":"pt-a1","active":true,"name":[{"family":"Stone"}]}"#,
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let read = call(app, "GET", "/Patient/pt-a1", &[]).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    let missed = call(app, "GET", "/Patient/pt-none", &[]).await;
    assert_eq!(missed.status, StatusCode::NOT_FOUND, "{}", missed.body);
}

fn parameter(body: &str, name: &str) -> Value {
    serde_json::from_str::<Value>(body)
        .expect("a parameters body")
        .get("parameter")
        .and_then(Value::as_array)
        .and_then(|held| {
            held.iter()
                .find(|entry| entry["name"] == name)
                .cloned()
        })
        .unwrap_or(Value::Null)
}

fn verified(body: &str) -> bool {
    parameter(body, "verified")["valueBoolean"] == json!(true)
}

fn fault(body: &str) -> String {
    parameter(body, "fault")["valueCode"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn stored(store: &Arc<MemoryStore>, sequence: u64) -> Value {
    let id = fhir_api::trail::identifier(sequence)
        .parse::<ResourceId>()
        .expect("a record id");
    let envelope = store.read(&id).await.expect("the record is stored");
    serde_json::from_slice(envelope.raw()).expect("a record body")
}

async fn replace(store: &Arc<MemoryStore>, body: &Value) {
    let bytes = serde_json::to_vec(body).expect("a record serializes");
    let envelope = ResourceEnvelope::parse(FhirVersion::R4, &bytes).expect("a record parses");
    store.update(envelope, None).await.expect("the record is replaced");
}

async fn count(store: &Arc<MemoryStore>) -> usize {
    store
        .search(&SearchQuery::of_type("AuditEvent".parse().unwrap()))
        .await
        .expect("the trail is searchable")
        .entries
        .len()
}

#[tokio::test]
async fn a_trail_the_service_wrote_verifies() {
    let (app, store, _) = recording();
    worked(&app).await;
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert_eq!(report.status, StatusCode::OK, "{}", report.body);
    assert!(verified(&report.body), "{}", report.body);
    assert_eq!(
        parameter(&report.body, "keyed")["valueBoolean"],
        json!(true),
        "{}",
        report.body
    );
    assert!(count(&store).await >= 3);
}

fn at<'a>(record: &'a mut Value, url: &str) -> &'a mut Value {
    record["extension"]
        .as_array_mut()
        .expect("a chained record")
        .iter_mut()
        .find(|held| held["url"] == json!(url))
        .expect("the named extension")
}

#[tokio::test]
async fn a_record_whose_sealed_content_changed_is_detected() {
    let (app, store, _) = recording();
    worked(&app).await;
    let mut record = stored(&store, 2).await;
    at(&mut record, "urn:fhir-service:audit-actor")["valueString"] = json!("someone-else");
    record["agent"][0]["who"]["identifier"]["value"] = json!("someone-else");
    replace(&store, &record).await;
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(!verified(&report.body), "{}", report.body);
    assert_eq!(fault(&report.body), "altered", "{}", report.body);
}

#[tokio::test]
async fn a_record_whose_digest_changed_is_detected() {
    let (app, store, _) = recording();
    worked(&app).await;
    let mut record = stored(&store, 2).await;
    at(&mut record, "urn:fhir-service:audit-digest")["valueString"] = json!(
        "1111111111111111111111111111111111111111111111111111111111111111"
    );
    replace(&store, &record).await;
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(!verified(&report.body), "{}", report.body);
    assert_eq!(fault(&report.body), "altered", "{}", report.body);
}

#[tokio::test]
async fn a_record_changed_beside_its_chain_is_detected() {
    let (app, store, _) = recording();
    worked(&app).await;
    let mut record = stored(&store, 2).await;
    record["agent"][0]["who"]["identifier"]["value"] = json!("someone-else");
    replace(&store, &record).await;
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(!verified(&report.body), "{}", report.body);
    assert_eq!(fault(&report.body), "rewritten", "{}", report.body);
}

#[tokio::test]
async fn a_deleted_record_is_detected_as_a_gap() {
    let (app, store, _) = recording();
    worked(&app).await;
    let id = fhir_api::trail::identifier(2)
        .parse::<ResourceId>()
        .expect("a record id");
    store.hard_delete(&id).await.expect("the record is removed");
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(!verified(&report.body), "{}", report.body);
    assert_eq!(fault(&report.body), "gap", "{}", report.body);
}

#[tokio::test]
async fn a_record_dropped_from_the_front_is_detected() {
    let (app, store, _) = recording();
    worked(&app).await;
    let id = fhir_api::trail::identifier(1)
        .parse::<ResourceId>()
        .expect("a record id");
    store.hard_delete(&id).await.expect("the record is removed");
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(!verified(&report.body), "{}", report.body);
    assert_eq!(fault(&report.body), "truncated", "{}", report.body);
}

#[tokio::test]
async fn a_record_dropped_from_the_end_is_detected_against_the_head_held_apart() {
    let (app, store, trail) = recording();
    worked(&app).await;
    let head = trail.head().expect("a head");
    let id = fhir_api::trail::identifier(head.sequence)
        .parse::<ResourceId>()
        .expect("a record id");
    store.hard_delete(&id).await.expect("the record is removed");
    let records = trail.collected().await.expect("the trail reads back");
    assert_eq!(
        fhir_store::trail::verify_against(trail.seal(), &records, &head),
        Err(fhir_store::Tamper::Truncated {
            expected: head.sequence,
            found: head.sequence - 1,
        })
    );
    assert!(
        fhir_store::trail::verify(trail.seal(), &records).is_ok(),
        "what survives is consistent with itself and only the held head says otherwise"
    );
}

#[tokio::test]
async fn a_rewrite_without_the_key_does_not_pass() {
    let (app, store, _) = recording();
    worked(&app).await;
    let forger = Seal::keyed("a-guess");
    let mut record = stored(&store, 2).await;
    record["agent"][0]["who"]["identifier"]["value"] = json!("someone-else");
    let sealed = fhir_api::trail::sealed_of(&record).expect("a chained record");
    let forged = fhir_store::trail::digest_of(&forger, &sealed);
    record["extension"][2]["valueString"] = json!(forged);
    replace(&store, &record).await;
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(!verified(&report.body), "{}", report.body);
}

#[tokio::test]
async fn retention_removes_what_is_older_and_the_rest_still_verifies() {
    let (app, store, _) = recording();
    worked(&app).await;
    let before = count(&store).await;
    let removal = call(
        &app,
        "POST",
        "/AuditEvent/$retain?_before=2030-01-01T00:00:00Z",
        &[],
    )
    .await;
    assert_eq!(removal.status, StatusCode::OK, "{}", removal.body);
    let removed = parameter(&removal.body, "removed")["valueUnsignedInt"]
        .as_u64()
        .unwrap_or_default();
    assert!(removed >= before as u64, "{}", removal.body);
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(verified(&report.body), "{}", report.body);
    assert!(count(&store).await < before + removed as usize);
}

#[tokio::test]
async fn a_retained_trail_that_loses_another_record_is_still_caught() {
    let (app, store, _) = recording();
    worked(&app).await;
    call(
        &app,
        "POST",
        "/AuditEvent/$retain?_before=2030-01-01T00:00:00Z",
        &[],
    )
    .await;
    let records = fhir_api::trail::collected(store.as_ref() as &dyn ResourceStore)
        .await
        .expect("the trail reads back");
    let middle = records[records.len() / 2].sequence;
    let id = fhir_api::trail::identifier(middle)
        .parse::<ResourceId>()
        .expect("a record id");
    store.hard_delete(&id).await.expect("the record is removed");
    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(!verified(&report.body), "{}", report.body);
}

#[tokio::test]
async fn a_horizon_covering_nothing_removes_nothing() {
    let (app, _, _) = recording();
    worked(&app).await;
    let removal = call(
        &app,
        "POST",
        "/AuditEvent/$retain?_before=2000-01-01T00:00:00Z",
        &[],
    )
    .await;
    assert_eq!(
        parameter(&removal.body, "removed")["valueUnsignedInt"],
        json!(0),
        "{}",
        removal.body
    );
}

#[tokio::test]
async fn retention_without_a_horizon_is_refused() {
    let (app, _, _) = recording();
    let removal = call(&app, "POST", "/AuditEvent/$retain", &[]).await;
    assert_eq!(removal.status, StatusCode::BAD_REQUEST, "{}", removal.body);
    let bad = call(&app, "POST", "/AuditEvent/$retain?_before=soon", &[]).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
}

#[tokio::test]
async fn an_export_carries_the_chain_and_no_record_content() {
    let (app, _, _) = recording();
    worked(&app).await;
    let exported = call(&app, "GET", "/AuditEvent/$export-trail", &[]).await;
    assert_eq!(exported.status, StatusCode::OK, "{}", exported.body);
    assert!(exported.body.lines().count() >= 3, "{}", exported.body);
    assert!(!exported.body.contains("Stone"), "{}", exported.body);
    assert!(!exported.body.contains("resourceType"), "{}", exported.body);
    for line in exported.body.lines() {
        let held: Value = serde_json::from_str(line).expect("a line is a record");
        assert!(held["sequence"].is_number(), "{line}");
        assert!(held["digest"].is_string(), "{line}");
        for name in held.as_object().expect("an object").keys() {
            assert!(
                [
                    "sequence", "previous", "digest", "kind", "actor", "client", "action",
                    "outcome", "recorded", "type", "id", "through", "anchor", "horizon",
                ]
                .contains(&name.as_str()),
                "{name} is not a published field"
            );
        }
    }
}

#[tokio::test]
async fn an_exported_trail_verifies_away_from_the_service() {
    let (app, _, trail) = recording();
    worked(&app).await;
    let exported = call(&app, "GET", "/AuditEvent/$export-trail", &[]).await;
    let mut expected = 0;
    let mut previous = fhir_store::trail::ORIGIN.to_owned();
    for line in exported.body.lines() {
        let held: Value = serde_json::from_str(line).expect("a line is a record");
        expected += 1;
        assert_eq!(held["sequence"], json!(expected), "{line}");
        assert_eq!(held["previous"], json!(previous), "{line}");
        previous = held["digest"].as_str().expect("a digest").to_owned();
    }
    assert!(expected >= 3, "{}", exported.body);
    let records = trail.collected().await.expect("the trail reads back");
    assert!(fhir_store::trail::verify(trail.seal(), &records).is_ok());
}

#[tokio::test]
async fn an_instance_that_starts_again_continues_the_chain_it_finds() {
    let (app, store, _) = recording();
    worked(&app).await;
    let held = Arc::clone(&store) as Arc<dyn ResourceStore>;
    let resumed = StoredTrail::resumed(held, FhirVersion::R4, Seal::keyed(KEY))
        .await
        .expect("a resumed trail");
    let before = resumed.head().expect("a head");
    let next = Service::new(
        Arc::clone(&store) as Arc<dyn ResourceStore>,
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
    .expect("an authorization is configured")
    .recording(Arc::new(resumed));
    let read = call(&next, "GET", "/Patient/pt-a1", &[]).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    let report = call(&next, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(verified(&report.body), "{}", report.body);
    assert!(
        parameter(&report.body, "sequence")["valueUnsignedInt"]
            .as_u64()
            .unwrap_or_default()
            > before.sequence
    );
}

#[tokio::test]
async fn a_trail_route_answers_nothing_without_a_credential() {
    let (app, _, _) = recording();
    let request = Request::builder()
        .method("GET")
        .uri("/AuditEvent/$verify")
        .header("host", "localhost")
        .body(Body::empty())
        .expect("a request");
    let response = app.router().oneshot(request).await.expect("a response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_instance_keeping_no_trail_routes_no_verification() {
    let store = Arc::new(MemoryStore::default());
    let app = Service::new(
        store as Arc<dyn ResourceStore>,
        FhirVersion::R4,
        vec![Dependency {
            name: "memory-store",
            check: Arc::new(|| Box::pin(async { Ok(()) })),
        }],
    );
    let request = Request::builder()
        .method("GET")
        .uri("/AuditEvent/$verify")
        .header("host", "localhost")
        .body(Body::empty())
        .expect("a request");
    let response = app.router().oneshot(request).await.expect("a response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_partial_horizon_removes_the_old_and_the_survivors_still_verify() {
    let (app, store, _) = recording();
    worked(&app).await;
    let horizon = stored(&store, 3).await["recorded"]
        .as_str()
        .expect("a record names when it was recorded")
        .to_owned();
    let later = call(&app, "GET", "/Patient/pt-a1", &[]).await;
    assert_eq!(later.status, StatusCode::OK, "{}", later.body);
    let before = count(&store).await;
    assert!(before >= 4, "the horizon must fall inside the trail: {before}");

    let removal = call(
        &app,
        "POST",
        &format!("/AuditEvent/$retain?_before={horizon}"),
        &[],
    )
    .await;
    assert_eq!(removal.status, StatusCode::OK, "{}", removal.body);
    let removed = parameter(&removal.body, "removed")["valueUnsignedInt"]
        .as_u64()
        .expect("a removal says how much it removed");
    assert_eq!(removed, 2, "only what lies before the horizon goes: {}", removal.body);

    assert!(count(&store).await > 0, "a partial horizon leaves survivors");

    let survivors = fhir_api::trail::collected(store.as_ref() as &dyn ResourceStore)
        .await
        .expect("the trail reads back");
    assert!(
        survivors.iter().all(|record| record.sequence > 2),
        "a removed record must not read back"
    );
    assert!(
        survivors.iter().any(|record| record.sequence == 3),
        "the record on the horizon stays"
    );

    let report = call(&app, "GET", "/AuditEvent/$verify", &[]).await;
    assert!(verified(&report.body), "{}", report.body);
}
