use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Authorization, HeldKeys, Roles, Service};
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

fn guarded(setting: &str) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let roles = Roles::parse(setting, FhirVersion::R4).expect("the setting reads");
    Service::new(Arc::new(store), FhirVersion::R4, Vec::new())
        .with_authorization(Authorization::new(
            ISSUER,
            "https://issuer.example.org/a",
            "https://issuer.example.org/t",
        ))
        .enforcing(Arc::new(HeldKeys::new(
            KeySet::parse(&signing().keys()).expect("a published key set"),
        )))
        .expect("an authorization is configured")
        .with_roles(roles)
}

fn token(scopes: &str, roles: &[&str]) -> String {
    signing().mint(&json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": scopes,
        "roles": roles,
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }))
}

async fn call(app: &Service, method: &str, uri: &str, bearer: &str, body: &[u8]) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .header("authorization", format!("Bearer {bearer}"))
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

fn patient(id: &str) -> Vec<u8> {
    json!({"resourceType": "Patient", "id": id, "active": true})
        .to_string()
        .into_bytes()
}

#[tokio::test]
async fn a_role_allowing_the_action_lets_the_request_through() {
    let app = guarded("clinician=read,write");
    let bearer = token("system/*.read system/*.write", &["clinician"]);
    let written = call(&app, "PUT", "/Patient/r-1", &bearer, &patient("r-1")).await;
    assert_eq!(written.status, StatusCode::CREATED, "{}", written.body);
    let read = call(&app, "GET", "/Patient/r-1", &bearer, b"").await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
}

#[tokio::test]
async fn a_role_withholding_the_action_refuses_it_even_where_the_scope_allows() {
    let app = guarded("reader=read");
    let bearer = token("system/*.read system/*.write", &["reader"]);
    let refused = call(&app, "PUT", "/Patient/r-2", &bearer, &patient("r-2")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert!(
        !refused.body.contains("reader") && !refused.body.contains("practitioner-1"),
        "the refusal names neither the role nor the token: {}",
        refused.body
    );
}

#[tokio::test]
async fn a_scope_withholding_the_action_refuses_it_even_where_the_role_allows() {
    let app = guarded("clinician=read,write");
    let bearer = token("system/*.read", &["clinician"]);
    let refused = call(&app, "PUT", "/Patient/r-3", &bearer, &patient("r-3")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
}

#[tokio::test]
async fn a_role_confined_to_a_type_does_not_reach_another() {
    let app = guarded("auditor=read:AuditEvent");
    let bearer = token("system/*.read", &["auditor"]);
    let refused = call(&app, "GET", "/Patient/r-4", &bearer, b"").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
}

#[tokio::test]
async fn a_token_carrying_no_recognised_role_is_refused_without_a_fallback() {
    let app = guarded("clinician=read,write");
    let bearer = token("system/*.read system/*.write", &["porter"]);
    let refused = call(&app, "GET", "/Patient/r-5", &bearer, b"").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
}

#[tokio::test]
async fn a_fallback_role_catches_a_token_no_role_matched() {
    let app = guarded("clinician=read,write;*=read");
    let bearer = token("system/*.read system/*.write", &["porter"]);
    let read = call(&app, "GET", "/Patient/r-6", &bearer, b"").await;
    assert_eq!(
        read.status,
        StatusCode::NOT_FOUND,
        "the read is allowed and the resource is simply absent: {}",
        read.body
    );
    let refused = call(&app, "PUT", "/Patient/r-6", &bearer, &patient("r-6")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
}

#[tokio::test]
async fn nothing_configured_leaves_the_scopes_to_decide_alone() {
    let app = guarded("");
    let bearer = token("system/*.read system/*.write", &[]);
    let written = call(&app, "PUT", "/Patient/r-7", &bearer, &patient("r-7")).await;
    assert_eq!(written.status, StatusCode::CREATED, "{}", written.body);
}

#[tokio::test]
async fn a_role_from_a_realm_access_claim_is_read() {
    let app = guarded("clinician=read,write");
    let bearer = signing().mint(&json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": "system/*.read system/*.write",
        "realm_access": {"roles": ["clinician"]},
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }));
    let written = call(&app, "PUT", "/Patient/r-8", &bearer, &patient("r-8")).await;
    assert_eq!(written.status, StatusCode::CREATED, "{}", written.body);
}

#[tokio::test]
async fn a_refusal_by_role_is_recorded_as_a_refusal() {
    let store = Arc::new(MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    })));
    let held = Arc::clone(&store) as Arc<dyn fhir_store::ResourceStore>;
    let app = Service::new(Arc::clone(&held), FhirVersion::R4, Vec::new())
        .with_authorization(Authorization::new(
            ISSUER,
            "https://issuer.example.org/a",
            "https://issuer.example.org/t",
        ))
        .enforcing(Arc::new(HeldKeys::new(
            KeySet::parse(&signing().keys()).expect("a published key set"),
        )))
        .expect("an authorization is configured")
        .with_roles(Roles::parse("reader=read", FhirVersion::R4).unwrap())
        .recording(Arc::new(
            fhir_api::StoredTrail::new(held, FhirVersion::R4)
                .sealed_with(fhir_store::Seal::keyed("a-key-the-store-never-sees")),
        ));
    let bearer = token("system/*.read system/*.write", &["reader"]);
    let refused = call(&app, "PUT", "/Patient/r-9", &bearer, &patient("r-9")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);

    let listed = call(&app, "GET", "/AuditEvent?_count=50", &bearer, b"").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let bundle: Value = serde_json::from_str(&listed.body).unwrap();
    let told = bundle["entry"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|entry| entry["resource"]["outcome"].as_str() == Some("8"))
                .count()
        })
        .unwrap_or_default();
    assert!(told >= 1, "a refusal is recorded: {}", listed.body);
}
