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

fn kept(version: &str) -> Value {
    let path = folder()
        .join("external")
        .join(format!("{}.json", version.to_ascii_lowercase()));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{version} has no report from outside this repository"));
    serde_json::from_str(&text).expect("the report is json")
}

fn severities(report: &Value, wanted: &[&str]) -> Vec<String> {
    report["entry"]
        .as_array()
        .map(|entries| entries.as_slice())
        .unwrap_or_default()
        .iter()
        .flat_map(|entry| {
            entry["resource"]["issue"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|issue| {
            issue["severity"]
                .as_str()
                .is_some_and(|held| wanted.contains(&held))
        })
        .map(|issue| issue["details"]["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn kept_with_terminology(version: &str) -> Value {
    let path = folder()
        .join("external")
        .join("tx")
        .join(format!("{}.json", version.to_ascii_lowercase()));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{version} has no report judged with a terminology server"));
    serde_json::from_str(&text).expect("the report is json")
}

#[test]
fn the_report_taken_with_a_terminology_server_carries_no_error() {
    for version in VERSIONS {
        let report = kept_with_terminology(version);
        assert_eq!(report["resourceType"], "Bundle", "{version}");
        let judged = report["entry"].as_array().map(Vec::len).unwrap_or_default();
        assert_eq!(judged, 11, "{version} was judged on {judged} answers");
        let failed = severities(&report, &["error", "fatal"]);
        assert!(failed.is_empty(), "{version}: {failed:?}");
    }
}

fn bulk_events(version: &str) -> Vec<Value> {
    let path = folder()
        .join("external")
        .join("bulk")
        .join(format!("{}.ndjson", version.to_ascii_lowercase()));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{version} has no bulk export report from outside"));
    text.lines()
        .map(|line| serde_json::from_str(line).expect("the report row is json"))
        .collect()
}

fn bulk_events_named(version: &str, event: &str) -> Vec<Value> {
    bulk_events(version)
        .into_iter()
        .filter(|row| row["eventId"] == event)
        .collect()
}

fn bulk_figure(row: &Value, name: &str) -> u64 {
    row["eventDetail"][name].as_u64().unwrap_or_default()
}

#[test]
fn the_bulk_export_the_published_client_drove_carries_no_error() {
    for version in VERSIONS {
        let kickoff = bulk_events_named(version, "kickoff");
        assert_eq!(kickoff.len(), 1, "{version} was kicked off {kickoff:?}");
        let url = kickoff[0]["eventDetail"]["exportUrl"]
            .as_str()
            .unwrap_or_default();
        assert!(url.ends_with("$export"), "{version} kicked off {url}");
        assert!(
            kickoff[0]["eventDetail"]["errorCode"].is_null(),
            "{version} kickoff failed: {}",
            kickoff[0]["eventDetail"]["errorBody"]
        );
        let pages = bulk_events_named(version, "status_page_complete");
        assert!(!pages.is_empty(), "{version} reported no manifest page");
        for page in &pages {
            assert_eq!(bulk_figure(page, "errorFileCount"), 0, "{version}: {page}");
        }
        let downloaded: u64 = bulk_events_named(version, "download_complete")
            .iter()
            .map(|row| bulk_figure(row, "resourceCount"))
            .sum();
        assert!(downloaded > 0, "{version} downloaded no resource");
        let complete = bulk_events_named(version, "export_complete");
        assert_eq!(complete.len(), 1, "{version} never completed: {complete:?}");
        assert!(
            bulk_figure(&complete[0], "resources") == downloaded,
            "{version} completed with {} of {downloaded} resources",
            bulk_figure(&complete[0], "resources")
        );
    }
}

#[test]
fn the_report_from_outside_names_every_version_and_carries_no_error() {
    for version in VERSIONS {
        let report = kept(version);
        assert_eq!(report["resourceType"], "Bundle", "{version}");
        let judged = report["entry"].as_array().map(Vec::len).unwrap_or_default();
        assert!(judged > 1, "{version} was judged on {judged} answers");
        let failed = severities(&report, &["error", "fatal"]);
        assert!(failed.is_empty(), "{version}: {failed:?}");
    }
}

fn script_reports(version: &str) -> Vec<Value> {
    let held = folder().join("external").join("scripts").join(version);
    let mut reports: Vec<Value> = Vec::new();
    for entry in std::fs::read_dir(&held).expect("the published suite kept its reports") {
        let path = entry.expect("a kept report is readable").path();
        if path.extension().and_then(|kind| kind.to_str()) != Some("json") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("a kept report is text");
        reports.push(json(&body));
    }
    reports
}

fn push_actions<'a>(held: &mut Vec<&'a Value>, holder: &'a Value) {
    if let Some(actions) = holder.get("action").and_then(Value::as_array) {
        held.extend(actions);
    }
}

fn script_actions<'a>(report: &'a Value, phase: &str) -> Vec<&'a Value> {
    let mut held: Vec<&Value> = Vec::new();
    match report.get(phase) {
        Some(Value::Array(items)) => {
            for item in items {
                push_actions(&mut held, item);
            }
        }
        Some(other) => push_actions(&mut held, other),
        None => {}
    }
    held
}

fn script_failures(report: &Value, name: &str) -> Vec<String> {
    let mut held: Vec<String> = Vec::new();
    for phase in ["setup", "test", "teardown"] {
        for action in script_actions(report, phase) {
            for kind in ["operation", "assert"] {
                let Some(action) = action.get(kind) else {
                    continue;
                };
                let result = action
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if result != "fail" && result != "error" {
                    continue;
                }
                let message = action
                    .get("message")
                    .or_else(|| action.get("description"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                held.push(format!("{name}: {}", message.replace('"', "")));
            }
        }
    }
    held
}

const SCRIPT_VERSIONS: [&str; 4] = ["stu3", "r4", "r4b", "r5"];

const SCRIPT_NAMES: [&str; 5] = [
    "testscript-example",
    "testscript-example-history",
    "testscript-example-readtest",
    "testscript-example-search",
    "testscript-example-update",
];

fn script_name(report: &Value) -> String {
    text(report.get("testScript").and_then(|held| held.get("reference")))
}

fn text(held: Option<&Value>) -> String {
    held.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn folded(held: &str) -> String {
    held.chars()
        .filter(|held| held.is_ascii_alphanumeric())
        .map(|held| held.to_ascii_lowercase())
        .collect()
}

fn script_id(report: &Value) -> Option<&'static str> {
    let held = folded(&script_name(report));
    SCRIPT_NAMES
        .iter()
        .copied()
        .find(|name| held.ends_with(&folded(name)))
}

fn recorded_failures(version: &str) -> &'static [&'static str] {
    match version {
        "stu3" => &SCRIPT_FAILURES_STU3,
        "r4" => &SCRIPT_FAILURES_R4,
        "r4b" => &SCRIPT_FAILURES_R4B,
        _ => &SCRIPT_FAILURES_R5,
    }
}

const SCRIPT_FAILURES_STU3: [&str; 5] = [
    "testscript-example-history: Response Code: Expected Response Code equals [200], but found [400].",
    "testscript-example-readtest: Content-Type: Expected Content-Type equals [xml], but found [application/fhir+json].",
    "testscript-example-readtest: Response: Expected Response equals [bad], but found [notFound].",
    "testscript-example-search: Navigation Links: Expected all navigation links, but did not receive.",
    "testscript-example-update: Response: Expected Response equals [okay], but found [bad].",
];

const SCRIPT_FAILURES_R4: [&str; 6] = [
    "testscript-example: Response Code: Expected Response Code equals [200], but found [201].",
    "testscript-example-history: Response Code: Expected Response Code equals [200], but found [400].",
    "testscript-example-readtest: Content-Type: Expected Content-Type equals [application/fhir+xml], but found [application/fhir+json].",
    "testscript-example-readtest: Response: Expected Response equals [bad], but found [notFound].",
    "testscript-example-search: Navigation Links: Expected all navigation links, but did not receive.",
    "testscript-example-update: Response: Expected Response equals [okay], but found [bad].",
];

const SCRIPT_FAILURES_R4B: [&str; 5] = [
    "testscript-example-history: Response Code: Expected Response Code equals [200], but found [400].",
    "testscript-example-readtest: Content-Type: Expected Content-Type equals [xml], but found [application/fhir+json].",
    "testscript-example-readtest: Response: Expected Response equals [bad], but found [notFound].",
    "testscript-example-search: Navigation Links: Expected all navigation links, but did not receive.",
    "testscript-example-update: Response: Expected Response equals [okay], but found [bad].",
];

const SCRIPT_FAILURES_R5: [&str; 5] = [
    "testscript-example-history: Response Code: Expected Response Code equals [200], but found [400].",
    "testscript-example-readtest: Content-Type: Expected Content-Type equals [xml], but found [application/fhir+json].",
    "testscript-example-readtest: Response: Expected Response equals [badRequest], but found [notFound].",
    "testscript-example-search: Navigation Links: Expected all navigation links, but did not receive.",
    "testscript-example-update: Response: Expected Response equals [okay], but found [bad].",
];

#[test]
fn the_published_test_scripts_a_running_instance_answered() {
    for version in SCRIPT_VERSIONS {
        let reports = script_reports(version);
        let mut named: Vec<String> = reports
            .iter()
            .filter_map(|report| report.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        named.sort();
        let mut ran: Vec<String> = reports.iter().filter_map(script_id).map(str::to_owned).collect();
        ran.sort();
        for name in SCRIPT_NAMES {
            assert!(
                ran.iter().any(|held| held == name),
                "{version}: the suite never ran {name}: {ran:?}"
            );
        }
        let kept = reports.len();
        assert_eq!(
            kept,
            SCRIPT_NAMES.len(),
            "{version}: the suite kept {kept} report(s): {named:?}"
        );
        let mut found: Vec<String> = reports
            .iter()
            .flat_map(|report| {
                let name = script_id(report).unwrap_or_default();
                script_failures(report, name)
            })
            .collect();
        found.sort();
        let mut known: Vec<String> = recorded_failures(version)
            .iter()
            .map(|line| line.to_string())
            .collect();
        known.sort();
        assert_eq!(
            found, known,
            "{version}: the suite judged the instance differently"
        );
    }
}
