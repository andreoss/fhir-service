mod live;

use live::{request, spawn_with, stop, Reply};
use serde_json::{json, Value};

const ISSUER: &str = "https://issuer.example.org";
const AUDIENCE: &str = "https://service.example.org";
const KEY: &str = "a-key-the-store-never-sees";
const SCOPES: &str = "system/*.read system/*.write system/*.export system/*.bulk-delete";

fn signing() -> &'static fhir_core::security::fixture::Issuer {
    use std::sync::OnceLock;
    static HELD: OnceLock<fhir_core::security::fixture::Issuer> = OnceLock::new();
    HELD.get_or_init(|| fhir_core::security::fixture::Issuer::generate("trail"))
}

fn token() -> String {
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64 + 300)
        .unwrap_or(0);
    signing().mint(&json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": "practitioner-1",
        "scope": SCOPES,
        "exp": expiry,
    }))
}

fn spawn_recording() -> (std::process::Child, u16) {
    let keys = signing().keys().to_string();
    spawn_with(&[
        ("FHIR_AUTH_ISSUER", ISSUER),
        ("FHIR_AUTH_AUDIENCE", AUDIENCE),
        (
            "FHIR_AUTH_AUTHORIZE",
            "https://issuer.example.org/authorize",
        ),
        ("FHIR_AUTH_TOKEN", "https://issuer.example.org/token"),
        ("FHIR_AUTH_KEYS", &keys),
        ("FHIR_AUDIT_KEY", KEY),
    ])
}

fn bearing(port: u16, method: &str, path: &str, body: &[u8]) -> Reply {
    let carried = format!("Bearer {}", token());
    let mut headers: Vec<(&str, &str)> = vec![("Authorization", &carried)];
    if !body.is_empty() {
        headers.push(("Content-Type", "application/fhir+json"));
    }
    request(port, method, path, &headers, body)
}

fn json_of(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| panic!("not a document: {body}"))
}

fn parameter(body: &str, name: &str) -> Value {
    json_of(body)["parameter"]
        .as_array()
        .and_then(|held| held.iter().find(|entry| entry["name"] == name).cloned())
        .unwrap_or(Value::Null)
}

fn verified(port: u16) -> Reply {
    bearing(port, "GET", "/AuditEvent/$verify", &[])
}

fn worked(port: u16) {
    let created = bearing(
        port,
        "POST",
        "/Patient",
        br#"{"resourceType":"Patient","id":"pt-s1","active":true,"name":[{"family":"Stone"}]}"#,
    );
    assert_eq!(created.status, 201, "{}", created.body);
    let read = bearing(port, "GET", "/Patient/pt-s1", &[]);
    assert_eq!(read.status, 200, "{}", read.body);
    let missed = bearing(port, "GET", "/Patient/pt-none", &[]);
    assert_eq!(missed.status, 404, "{}", missed.body);
}

#[test]
fn live_a_running_instance_keeps_a_trail_that_verifies() {
    let (child, port) = spawn_recording();
    worked(port);
    let report = verified(port);
    stop(child);
    assert_eq!(report.status, 200, "{}", report.body);
    assert_eq!(
        parameter(&report.body, "verified")["valueBoolean"],
        json!(true),
        "{}",
        report.body
    );
    assert_eq!(
        parameter(&report.body, "keyed")["valueBoolean"],
        json!(true),
        "{}",
        report.body
    );
    assert!(
        parameter(&report.body, "sequence")["valueUnsignedInt"]
            .as_u64()
            .unwrap_or_default()
            >= 3,
        "{}",
        report.body
    );
}

fn altered(port: u16, url: &str, value: &str) -> Reply {
    let held = bearing(port, "GET", "/AuditEvent/au-000000000002", &[]);
    assert_eq!(held.status, 200, "{}", held.body);
    let mut record = json_of(&held.body);
    let carried = record["extension"]
        .as_array_mut()
        .expect("a chained record")
        .iter_mut()
        .find(|entry| entry["url"] == json!(url))
        .expect("the named extension");
    carried["valueString"] = json!(value);
    if url.ends_with("audit-actor") {
        record["agent"][0]["who"]["identifier"]["value"] = json!(value);
    }
    let written = bearing(
        port,
        "PUT",
        "/AuditEvent/au-000000000002",
        serde_json::to_vec(&record).expect("a record").as_slice(),
    );
    assert_eq!(written.status, 200, "{}", written.body);
    verified(port)
}

#[test]
fn live_a_record_whose_sealed_content_changed_is_detected() {
    let (child, port) = spawn_recording();
    worked(port);
    let report = altered(port, "urn:fhir-service:audit-actor", "someone-else");
    stop(child);
    assert_eq!(
        parameter(&report.body, "verified")["valueBoolean"],
        json!(false),
        "{}",
        report.body
    );
    assert_eq!(
        parameter(&report.body, "fault")["valueCode"],
        json!("altered"),
        "{}",
        report.body
    );
}

#[test]
fn live_a_record_changed_beside_its_chain_is_detected() {
    let (child, port) = spawn_recording();
    worked(port);
    let held = bearing(port, "GET", "/AuditEvent/au-000000000002", &[]);
    assert_eq!(held.status, 200, "{}", held.body);
    let mut record = json_of(&held.body);
    record["agent"][0]["who"]["identifier"]["value"] = json!("someone-else");
    let written = bearing(
        port,
        "PUT",
        "/AuditEvent/au-000000000002",
        serde_json::to_vec(&record).expect("a record").as_slice(),
    );
    assert_eq!(written.status, 200, "{}", written.body);
    let report = verified(port);
    stop(child);
    assert_eq!(
        parameter(&report.body, "verified")["valueBoolean"],
        json!(false),
        "{}",
        report.body
    );
    assert_eq!(
        parameter(&report.body, "fault")["valueCode"],
        json!("rewritten"),
        "{}",
        report.body
    );
}

#[test]
fn live_a_record_removed_through_the_surface_is_detected_as_a_gap() {
    let (child, port) = spawn_recording();
    worked(port);
    let removed = bearing(port, "DELETE", "/AuditEvent/au-000000000002", &[]);
    assert!(
        removed.status == 200 || removed.status == 204,
        "{}",
        removed.body
    );
    let report = verified(port);
    stop(child);
    assert_eq!(
        parameter(&report.body, "verified")["valueBoolean"],
        json!(false),
        "{}",
        report.body
    );
    assert_eq!(
        parameter(&report.body, "fault")["valueCode"],
        json!("gap"),
        "{}",
        report.body
    );
}

#[test]
fn live_the_trail_exports_with_its_chain_and_no_record_content() {
    let (child, port) = spawn_recording();
    worked(port);
    let exported = bearing(port, "GET", "/AuditEvent/$export-trail", &[]);
    stop(child);
    assert_eq!(exported.status, 200, "{}", exported.body);
    assert!(exported.body.lines().count() >= 3, "{}", exported.body);
    assert!(!exported.body.contains("Stone"), "{}", exported.body);
    let mut expected = 0;
    let mut previous = "0".repeat(64);
    for line in exported.body.lines() {
        let held = json_of(line);
        expected += 1;
        assert_eq!(held["sequence"], json!(expected), "{line}");
        assert_eq!(held["previous"], json!(previous), "{line}");
        previous = held["digest"].as_str().expect("a digest").to_owned();
    }
}

#[test]
fn live_retention_removes_the_old_and_leaves_a_trail_that_still_verifies() {
    let (child, port) = spawn_recording();
    worked(port);
    let removal = bearing(
        port,
        "POST",
        "/AuditEvent/$retain?_before=2030-01-01T00:00:00Z",
        &[],
    );
    let report = verified(port);
    stop(child);
    assert_eq!(removal.status, 200, "{}", removal.body);
    assert!(
        parameter(&removal.body, "removed")["valueUnsignedInt"]
            .as_u64()
            .unwrap_or_default()
            >= 3,
        "{}",
        removal.body
    );
    assert_eq!(
        parameter(&report.body, "verified")["valueBoolean"],
        json!(true),
        "{}",
        report.body
    );
}

#[test]
fn live_a_trail_route_answers_nothing_without_a_credential() {
    let (child, port) = spawn_recording();
    let report = request(port, "GET", "/AuditEvent/$verify", &[], &[]);
    stop(child);
    assert_eq!(report.status, 401, "{}", report.body);
}
