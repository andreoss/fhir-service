use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Authorization, HeldKeys, Policies, Service};
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

fn guarded(policies: Policies) -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
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
        .with_policies(policies)
}

fn token(scopes: &str, user: Option<&str>) -> String {
    let mut payload = json!({
        "iss": ISSUER,
        "sub": "practitioner-1",
        "scope": scopes,
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    });
    if let Some(user) = user {
        payload["fhirUser"] = json!(user);
    }
    signing().mint(&payload)
}

async fn call(
    app: &Service,
    method: &str,
    uri: &str,
    scopes: &str,
    user: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, String) {
    let held = match &body {
        Some(body) => Body::from(serde_json::to_vec(body).expect("a body serialises")),
        None => Body::empty(),
    };
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/fhir+json")
        .header("authorization", format!("Bearer {}", token(scopes, user)))
        .body(held)
        .expect("a request is built");
    let response = app.router().oneshot(request).await.expect("an answer");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body is read")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn policy(id: &str, subjects: Value, scopes: Value) -> Value {
    json!({
        "resourceType": "AccessPolicy",
        "id": id,
        "subject": subjects,
        "scope": scopes,
    })
}

const WIDE: &str = "system/*.read system/*.write";

async fn holding(app: &Service, policies: &[Value]) {
    for held in policies {
        let id = held["id"].as_str().expect("a policy has an id");
        let (status, body) = call(
            app,
            "PUT",
            &format!("/AccessPolicy/{id}"),
            WIDE,
            None,
            Some(held.clone()),
        )
        .await;
        assert!(
            status.is_success(),
            "a policy is written like any other resource: {status} {body}"
        );
    }
}

#[tokio::test]
async fn a_policy_narrows_the_token_that_carries_more() {
    let app = guarded(Policies::on().expect("policies are registered"));
    holding(
        &app,
        &[policy(
            "p-one",
            json!(["Practitioner/p1"]),
            json!(["user/Observation.read"]),
        )],
    )
    .await;

    let (read, body) = call(
        &app,
        "GET",
        "/Observation",
        WIDE,
        Some("Practitioner/p1"),
        None,
    )
    .await;
    assert_eq!(read, StatusCode::OK, "what the policy allows: {body}");

    let (written, body) = call(
        &app,
        "POST",
        "/Observation",
        WIDE,
        Some("Practitioner/p1"),
        Some(json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"}})),
    )
    .await;
    assert_eq!(
        written,
        StatusCode::FORBIDDEN,
        "and not what it does not, though the token carries it: {body}"
    );

    let (other, _) = call(&app, "GET", "/Patient", WIDE, Some("Practitioner/p1"), None).await;
    assert_eq!(other, StatusCode::FORBIDDEN, "nor another type");
}

#[tokio::test]
async fn a_policy_cannot_widen_a_token() {
    let app = guarded(Policies::on().expect("policies are registered"));
    holding(
        &app,
        &[policy(
            "p-wide",
            json!(["Practitioner/p1"]),
            json!(["system/*.read", "system/*.write"]),
        )],
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        "/Observation",
        "system/Observation.read",
        Some("Practitioner/p1"),
        Some(json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"}})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the token carries read only, and a policy allowing writes does not \
         give it one: {body}"
    );
}

#[tokio::test]
async fn a_user_no_policy_names_is_not_restricted_by_one() {
    let app = guarded(Policies::on().expect("policies are registered"));
    holding(
        &app,
        &[policy(
            "p-one",
            json!(["Practitioner/p1"]),
            json!(["user/Observation.read"]),
        )],
    )
    .await;
    let (status, body) = call(&app, "GET", "/Patient", WIDE, Some("Practitioner/p2"), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the first policy written does not shut out everybody else: {body}"
    );
}

#[tokio::test]
async fn a_token_naming_no_user_is_not_restricted_by_a_policy() {
    let app = guarded(Policies::on().expect("policies are registered"));
    holding(
        &app,
        &[policy(
            "p-one",
            json!(["Practitioner/p1"]),
            json!(["user/Observation.read"]),
        )],
    )
    .await;
    let (status, _) = call(&app, "GET", "/Patient", WIDE, None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a policy names users, and a token naming none is named by none"
    );
}

#[tokio::test]
async fn two_policies_naming_one_user_are_taken_together() {
    let app = guarded(Policies::on().expect("policies are registered"));
    holding(
        &app,
        &[
            policy(
                "p-read",
                json!(["Practitioner/p1"]),
                json!(["user/Observation.read"]),
            ),
            policy(
                "p-patient",
                json!([{"reference": "Practitioner/p1"}]),
                json!(["user/Patient.read"]),
            ),
        ],
    )
    .await;
    for kind in ["Observation", "Patient"] {
        let (status, body) = call(
            &app,
            "GET",
            &format!("/{kind}"),
            WIDE,
            Some("Practitioner/p1"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{kind}: {body}");
    }
    let (refused, _) = call(
        &app,
        "GET",
        "/Condition",
        WIDE,
        Some("Practitioner/p1"),
        None,
    )
    .await;
    assert_eq!(
        refused,
        StatusCode::FORBIDDEN,
        "what neither policy allows is allowed by neither"
    );
}

#[tokio::test]
async fn an_absolute_user_reference_is_the_user_the_policy_names() {
    let app = guarded(Policies::on().expect("policies are registered"));
    holding(
        &app,
        &[policy(
            "p-one",
            json!(["Practitioner/p1"]),
            json!(["user/Observation.read"]),
        )],
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        "/Patient",
        WIDE,
        Some("https://ehr.example.org/fhir/Practitioner/p1"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the issuer writes the user as a URL and the operator writes it \
         relatively; they are the same person: {body}"
    );
}

#[tokio::test]
async fn a_refusal_says_nothing_of_the_policy() {
    let app = guarded(Policies::on().expect("policies are registered"));
    holding(
        &app,
        &[policy(
            "p-secret",
            json!(["Practitioner/p1"]),
            json!(["user/Observation.read"]),
        )],
    )
    .await;
    let (status, body) = call(&app, "GET", "/Patient", WIDE, Some("Practitioner/p1"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body.contains("Patient") && !body.contains("p-secret") && !body.contains("Practitioner/p1"),
        "the refusal names the action and the type and nothing else: {body}"
    );
}

#[tokio::test]
async fn an_instance_consulting_no_policies_is_unchanged() {
    let app = guarded(Policies::off());
    let (status, body) = call(&app, "GET", "/Patient", WIDE, Some("Practitioner/p1"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
