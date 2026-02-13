mod live;

use live::{request, spawn_with, stop};
use serde_json::Value;
use std::path::PathBuf;

const VERSIONS: [&str; 4] = ["STU3", "R4", "R4B", "R5"];
const RECORD: &str = "FHIR_CONFORMANCE_RECORD";

const AUTHORIZED: [(&str, &str); 5] = [
    (
        "FHIR_AUTH_CAPABILITIES",
        "launch-standalone,client-public,sso-openid-connect,permission-v2",
    ),
    ("FHIR_AUTH_ISSUER", "https://issuer.example.org"),
    ("FHIR_AUTH_AUTHORIZE", "https://issuer.example.org/authorize"),
    ("FHIR_AUTH_TOKEN", "https://issuer.example.org/token"),
    ("FHIR_AUTH_SCOPES", "system/*.read,system/*.write"),
];

fn folder() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("doc")
        .join("conformance")
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).expect("the answer is a document")
}

fn named(value: &Value, path: &[&str]) -> Vec<String> {
    let mut held: Vec<String> = value
        .pointer(&format!("/{}", path.join("/")))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("code")
                        .or_else(|| item.get("name"))
                        .or_else(|| item.get("type"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    held.sort();
    held.dedup();
    held
}

fn digest(version: &str, statement: &Value, discovery: &Value) -> String {
    let types = named(statement, &["rest", "0", "resource"]);
    let interactions = named(statement, &["rest", "0", "interaction"]);
    let params = named(statement, &["rest", "0", "searchParam"]);
    let operations = named(statement, &["rest", "0", "operation"]);
    let mut capabilities: Vec<String> = discovery
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    capabilities.sort();
    let numbered = statement
        .get("fhirVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("= Conformance record {version}\n\n"));
    out.push_str(&format!("version: {numbered}\n"));
    out.push_str(&format!("resource types: {}\n", types.len()));
    out.push_str(&format!("system interactions: {}\n", interactions.join(", ")));
    out.push_str(&format!("common search parameters: {}\n", params.len()));
    out.push_str(&format!("operations: {}\n", operations.join(", ")));
    out.push_str(&format!("smart capabilities: {}\n", capabilities.join(", ")));
    out.push_str(&format!(
        "token endpoint: {}\n",
        discovery
            .get("token_endpoint")
            .and_then(Value::as_str)
            .unwrap_or_default()
    ));
    out
}

fn observed(version: &str) -> String {
    let mut env: Vec<(&str, &str)> = vec![("FHIR_VERSION", version)];
    env.extend(AUTHORIZED);
    let (child, port) = spawn_with(&env);
    let statement = request(port, "GET", "/metadata", &[], &[]);
    let discovery = request(port, "GET", "/.well-known/smart-configuration", &[], &[]);
    stop(child);
    assert_eq!(statement.status, 200, "{version}: {}", statement.body);
    assert_eq!(discovery.status, 200, "{version}: {}", discovery.body);
    digest(version, &json(&statement.body), &json(&discovery.body))
}

#[test]
fn live_the_recorded_conformance_of_every_version_still_holds() {
    let recording = std::env::var(RECORD).is_ok();
    for version in VERSIONS {
        let found = observed(version);
        let path = folder().join(format!("{}.adoc", version.to_ascii_lowercase()));
        if recording {
            std::fs::create_dir_all(folder()).expect("the record folder is writable");
            std::fs::write(&path, &found).expect("the record is written");
            continue;
        }
        let held = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("{version} has no recorded conformance"));
        assert_eq!(held, found, "{version} drifted from its record");
    }
}

#[test]
fn the_suites_that_remain_unproven_are_named() {
    let path = folder().join("unproven.adoc");
    let held = std::fs::read_to_string(&path).expect("the unproven record is present");
    assert!(held.contains("unproven"), "{held}");
    for version in VERSIONS {
        assert!(held.contains(version), "{version} is not named");
    }
}
