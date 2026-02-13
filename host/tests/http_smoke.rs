mod live;

use live::{request, spawn_server, spawn_with, stop, Reply};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;


fn header<'a>(reply: &'a Reply, name: &str) -> &'a str {
    reply
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn issue_code(body: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body).expect("body must be json");
    value["issue"][0]["code"].as_str().unwrap_or_default().to_owned()
}

fn patient(id: &str, active: bool) -> Vec<u8> {
    format!(r#"{{"resourceType":"Patient","id":"{id}","active":{active}}}"#).into_bytes()
}

#[test]
fn health_endpoint_reports_ok() {
    let (child, port) = spawn_server();
    let reply = request(port, "GET", "/health", &[], &[]);
    stop(child);
    assert_eq!(reply.status, 200);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["status"], "ok");
    assert_eq!(value["dependencies"][0]["name"], "store");
    assert_eq!(value["dependencies"][0]["status"], "ok");
}

#[test]
fn full_interaction_chain_over_http() {
    let (child, port) = spawn_server();

    let created = request(port, "POST", "/Patient", &[], &patient("pt-1", true));
    assert_eq!(created.status, 201, "create failed: {}", created.body);
    assert_eq!(header(&created, "etag"), "W/\"1\"");
    assert!(header(&created, "location").contains("/Patient/pt-1/_history/1"));
    assert!(header(&created, "content-location").contains("/Patient/pt-1/_history/1"));
    assert!(header(&created, "last-modified").ends_with(" GMT"), "last-modified was {}", header(&created, "last-modified"));
    let value: serde_json::Value = serde_json::from_str(&created.body).unwrap();
    assert_eq!(value["meta"]["versionId"], "1");
    assert_eq!(value["active"], true);

    let read = request(port, "GET", "/Patient/pt-1", &[], &[]);
    assert_eq!(read.status, 200);
    assert_eq!(header(&read, "etag"), "W/\"1\"");
    assert_eq!(header(&read, "content-type"), "application/fhir+json");

    let updated = request(port, "PUT", "/Patient/pt-1", &[("if-match", "W/\"1\"")], &patient("pt-1", false));
    assert_eq!(updated.status, 200, "update failed: {}", updated.body);
    assert_eq!(header(&updated, "etag"), "W/\"2\"");
    assert!(header(&updated, "content-location").contains("/Patient/pt-1/_history/2"));

    let v1 = request(port, "GET", "/Patient/pt-1/_history/1", &[], &[]);
    assert_eq!(v1.status, 200);
    assert_eq!(header(&v1, "etag"), "W/\"1\"");
    assert!(v1.body.contains("\"active\":true"));

    let read = request(port, "GET", "/Patient/pt-1", &[], &[]);
    assert_eq!(header(&read, "etag"), "W/\"2\"");

    stop(child);
}

#[test]
fn stale_if_match_is_conflict_with_outcome() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-2", true));
    request(port, "PUT", "/Patient/pt-2", &[("if-match", "W/\"1\"")], &patient("pt-2", false));
    let reply = request(port, "PUT", "/Patient/pt-2", &[("if-match", "W/\"1\"")], &patient("pt-2", true));
    stop(child);
    assert_eq!(reply.status, 409);
    assert_eq!(issue_code(&reply.body), "conflict");
    assert!(reply.body.contains("OperationOutcome"));
}

#[test]
fn duplicate_create_is_conflict_with_outcome() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-3", true));
    let reply = request(port, "POST", "/Patient", &[], &patient("pt-3", true));
    stop(child);
    assert_eq!(reply.status, 409);
    assert_eq!(issue_code(&reply.body), "duplicate");
}

#[test]
fn noop_update_never_advances_the_version() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-4", true));
    let reply = request(port, "PUT", "/Patient/pt-4", &[("if-match", "W/\"1\"")], &patient("pt-4", true));
    assert_eq!(reply.status, 200);
    assert_eq!(header(&reply, "etag"), "W/\"1\"", "no-op update must not advance the version");
    stop(child);
}

#[test]
fn create_without_id_assigns_server_id() {
    let (child, port) = spawn_server();
    let body = br#"{"resourceType":"Patient","active":true}"#.to_vec();
    let reply = request(port, "POST", "/Patient", &[], &body);
    stop(child);
    assert_eq!(reply.status, 201);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    let id = value["id"].as_str().expect("server id must be present");
    assert!(!id.is_empty());
    assert!(header(&reply, "location").ends_with(&format!("/Patient/{id}/_history/1")));
}

#[test]
fn read_unknown_id_returns_outcome_404() {
    let (child, port) = spawn_server();
    let reply = request(port, "GET", "/Patient/nobody", &[], &[]);
    stop(child);
    assert_eq!(reply.status, 404);
    assert_eq!(issue_code(&reply.body), "not-found");
}

#[test]
fn vread_unknown_version_returns_outcome_404() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-5", true));
    let reply = request(port, "GET", "/Patient/pt-5/_history/99", &[], &[]);
    stop(child);
    assert_eq!(reply.status, 404);
    assert_eq!(issue_code(&reply.body), "not-found");
}

#[test]
fn malformed_id_returns_outcome_400() {
    let (child, port) = spawn_server();
    let reply = request(port, "GET", "/Patient/bad%20id", &[], &[]);
    stop(child);
    assert_eq!(reply.status, 400);
    assert_eq!(issue_code(&reply.body), "invalid");
}

#[test]
fn body_type_mismatch_is_rejected() {
    let (child, port) = spawn_server();
    let reply = request(port, "POST", "/Patient", &[], &patient("pt-6", true));
    assert_eq!(reply.status, 201);
    let mismatch = request(port, "GET", "/Observation/pt-6", &[], &[]);
    assert_eq!(mismatch.status, 404);
    let bad_create = request(port, "POST", "/Patient", &[], b"{\"resourceType\":\"Observation\",\"id\":\"o-1\"}");
    stop(child);
    assert_eq!(bad_create.status, 400);
    assert_eq!(issue_code(&bad_create.body), "invalid");
}

#[test]
fn unsupported_method_returns_outcome_405() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-7", true));
    let reply = request(port, "POST", "/Patient/pt-7", &[], &[]);
    stop(child);
    assert_eq!(reply.status, 405);
    assert_eq!(issue_code(&reply.body), "not-allowed");
    assert!(reply.body.contains("OperationOutcome"));
}

#[test]
fn conditional_create_returns_the_existing_match() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-c1", true));
    let reply = request(
        port,
        "POST",
        "/Patient",
        &[("If-None-Exist", "_id=pt-c1")],
        &patient("pt-c2", true),
    );
    let absent = request(port, "GET", "/Patient/pt-c2", &[], &[]);
    stop(child);
    assert_eq!(reply.status, 200, "conditional create failed: {}", reply.body);
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["id"], "pt-c1");
    assert_eq!(absent.status, 404);
}

#[test]
fn conditional_update_creates_then_updates_the_match() {
    let (child, port) = spawn_server();
    let created = request(port, "PUT", "/Patient?_id=pt-c3", &[], &patient("pt-c3", true));
    let updated = request(port, "PUT", "/Patient?_id=pt-c3", &[], &patient("pt-c3", false));
    stop(child);
    assert_eq!(created.status, 201, "conditional create failed: {}", created.body);
    assert_eq!(header(&created, "etag"), "W/\"1\"");
    assert_eq!(updated.status, 200, "conditional update failed: {}", updated.body);
    assert_eq!(header(&updated, "etag"), "W/\"2\"");
}

#[test]
fn conditional_update_with_many_matches_is_precondition_failed() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-c4", true));
    request(port, "POST", "/Patient", &[], &patient("pt-c5", true));
    let reply = request(port, "PUT", "/Patient?active=true", &[], &patient("pt-c4", false));
    stop(child);
    assert_eq!(reply.status, 412, "body was {}", reply.body);
    assert_eq!(issue_code(&reply.body), "multiple-matches");
}

#[test]
fn delete_then_read_reports_the_resource_as_gone() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-d1", true));
    let deleted = request(port, "DELETE", "/Patient/pt-d1", &[], &[]);
    let read = request(port, "GET", "/Patient/pt-d1", &[], &[]);
    let old = request(port, "GET", "/Patient/pt-d1/_history/1", &[], &[]);
    stop(child);
    assert_eq!(deleted.status, 204, "delete failed: {}", deleted.body);
    assert_eq!(header(&deleted, "etag"), "W/\"2\"");
    assert_eq!(read.status, 410, "read said {}", read.body);
    assert_eq!(issue_code(&read.body), "deleted");
    assert_eq!(old.status, 200, "earlier version must stay readable");
}

#[test]
fn hard_delete_and_purge_history_remove_versions() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-d2", true));
    request(port, "PUT", "/Patient/pt-d2", &[], &patient("pt-d2", false));
    let purged = request(port, "POST", "/Patient/pt-d2/$purge-history", &[], &[]);
    let gone = request(port, "GET", "/Patient/pt-d2/_history/1", &[], &[]);
    let hard = request(port, "DELETE", "/Patient/pt-d2?_hardDelete=true", &[], &[]);
    let after = request(port, "GET", "/Patient/pt-d2", &[], &[]);
    stop(child);
    assert_eq!(purged.status, 200, "purge failed: {}", purged.body);
    let value: serde_json::Value = serde_json::from_str(&purged.body).unwrap();
    assert_eq!(value["parameter"][0]["valueInteger"], 1);
    assert_eq!(gone.status, 404);
    assert_eq!(hard.status, 204);
    assert_eq!(after.status, 404);
}

#[test]
fn conditional_delete_removes_the_single_match() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-d3", true));
    let deleted = request(port, "DELETE", "/Patient?_id=pt-d3", &[], &[]);
    let read = request(port, "GET", "/Patient/pt-d3", &[], &[]);
    let again = request(port, "DELETE", "/Patient?_id=pt-none", &[], &[]);
    stop(child);
    assert_eq!(deleted.status, 204, "conditional delete failed: {}", deleted.body);
    assert_eq!(read.status, 410);
    assert_eq!(again.status, 404);
}

#[test]
fn json_patch_updates_the_resource_over_http() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-p1", true));
    let patched = request(
        port,
        "PATCH",
        "/Patient/pt-p1",
        &[("Content-Type", "application/json-patch+json")],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    );
    let rejected = request(
        port,
        "PATCH",
        "/Patient/pt-p1",
        &[("Content-Type", "application/json-patch+json")],
        br#"[{"op":"remove","path":"/gender"}]"#,
    );
    let read = request(port, "GET", "/Patient/pt-p1", &[], &[]);
    stop(child);
    assert_eq!(patched.status, 200, "patch failed: {}", patched.body);
    assert_eq!(header(&patched, "etag"), "W/\"2\"");
    assert_eq!(rejected.status, 400);
    assert_eq!(issue_code(&rejected.body), "invalid");
    let value: serde_json::Value = serde_json::from_str(&read.body).unwrap();
    assert_eq!(value["meta"]["versionId"], "2", "a rejected patch must write nothing");
    assert_eq!(value["active"], false);
}

#[test]
fn path_patch_updates_the_resource_over_http() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-p2", true));
    let body = br#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[
        {"name":"type","valueCode":"replace"},
        {"name":"path","valueString":"Patient.active"},
        {"name":"value","valueBoolean":false}]}]}"#;
    let patched = request(port, "PATCH", "/Patient?_id=pt-p2", &[("Content-Type", "application/fhir+json")], body);
    stop(child);
    assert_eq!(patched.status, 200, "conditional patch failed: {}", patched.body);
    let value: serde_json::Value = serde_json::from_str(&patched.body).unwrap();
    assert_eq!(value["active"], false);
}

#[test]
fn history_over_http_pages_and_orders_versions() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &patient("pt-h1", true));
    request(port, "PUT", "/Patient/pt-h1", &[], &patient("pt-h1", false));
    request(port, "DELETE", "/Patient/pt-h1", &[], &[]);
    let instance = request(port, "GET", "/Patient/pt-h1/_history", &[], &[]);
    let typed = request(port, "GET", "/Patient/_history?_count=2", &[], &[]);
    let system = request(port, "GET", "/_history?_summary=count", &[], &[]);
    let oldest = request(port, "GET", "/Patient/pt-h1/_history?_sort=_lastUpdated", &[], &[]);
    let unknown = request(port, "GET", "/Patient/pt-none/_history", &[], &[]);
    let rejected = request(port, "GET", "/_history?_sort=name", &[], &[]);
    stop(child);

    assert_eq!(instance.status, 200, "instance history failed: {}", instance.body);
    assert_eq!(header(&instance, "content-type"), "application/fhir+json");
    let value: serde_json::Value = serde_json::from_str(&instance.body).unwrap();
    assert_eq!(value["resourceType"], "Bundle");
    assert_eq!(value["type"], "history");
    assert_eq!(value["total"], 3);
    assert_eq!(value["entry"][0]["request"]["method"], "DELETE");
    assert_eq!(value["entry"][1]["request"]["method"], "PUT");
    assert_eq!(value["entry"][2]["request"]["method"], "POST");
    assert_eq!(value["entry"][2]["response"]["etag"], "W/\"1\"");

    let typed: serde_json::Value = serde_json::from_str(&typed.body).unwrap();
    assert_eq!(typed["entry"].as_array().unwrap().len(), 2);
    let next = typed["link"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["relation"] == "next")
        .expect("a further page must be linked");
    assert!(next["url"].as_str().unwrap().contains("ct="));

    let system: serde_json::Value = serde_json::from_str(&system.body).unwrap();
    assert_eq!(system["total"], 3);
    assert!(system["entry"].is_null());

    let oldest: serde_json::Value = serde_json::from_str(&oldest.body).unwrap();
    assert_eq!(oldest["entry"][0]["response"]["etag"], "W/\"1\"");

    assert_eq!(unknown.status, 404);
    assert_eq!(rejected.status, 400);
    assert_eq!(issue_code(&rejected.body), "not-supported");
}

fn ids(body: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body).expect("bundle must be json");
    value["entry"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["resource"]["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn total(body: &str) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_str(body).expect("bundle must be json");
    value["total"].clone()
}

fn next_token(body: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body).expect("bundle must be json");
    value["link"]
        .as_array()
        .and_then(|links| {
            links
                .iter()
                .find(|link| link["relation"] == "next")
                .and_then(|link| link["url"].as_str())
        })
        .and_then(|url| url.rsplit("ct=").next())
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn live_search_selects_pages_and_reports_totals() {
    let (child, port) = spawn_server();
    for index in 1..=5 {
        request(port, "POST", "/Patient", &[], &patient(&format!("pt-k{index}"), index % 2 == 1));
    }
    let all = request(port, "GET", "/Patient", &[], &[]);
    let by_id = request(port, "GET", "/Patient?_id=pt-k3", &[], &[]);
    let active = request(port, "GET", "/Patient?active=true", &[], &[]);
    let first = request(port, "GET", "/Patient?_count=2&_sort=_id", &[], &[]);
    let token = next_token(&first.body);
    let second = request(port, "GET", &format!("/Patient?_count=2&_sort=_id&ct={token}"), &[], &[]);
    let counted = request(port, "GET", "/Patient?_summary=count", &[], &[]);
    let untotalled = request(port, "GET", "/Patient?_total=none", &[], &[]);
    stop(child);

    assert_eq!(all.status, 200);
    assert_eq!(header(&all, "content-type"), "application/fhir+json");
    assert_eq!(total(&all.body), 5);
    assert_eq!(ids(&by_id.body), vec!["pt-k3".to_owned()]);
    assert_eq!(total(&active.body), 3);
    assert_eq!(ids(&first.body), vec!["pt-k1".to_owned(), "pt-k2".to_owned()]);
    assert!(!token.is_empty(), "no continuation token was offered");
    assert_eq!(ids(&second.body), vec!["pt-k3".to_owned(), "pt-k4".to_owned()]);
    assert_eq!(total(&counted.body), 5);
    assert!(ids(&counted.body).is_empty());
    assert!(total(&untotalled.body).is_null());
}

#[test]
fn live_search_matches_typed_values_and_rejects_the_unsupported() {
    let (child, port) = spawn_server();
    let observation = br#"{"resourceType":"Observation","id":"ob-k1","status":"final","code":{"text":"probe"},"code":{"coding":[{"system":"http://loinc.org","code":"8867-4"}]},"subject":{"reference":"Patient/pt-k1"},"effectiveDateTime":"2026-09-06T04:00:00Z","valueQuantity":{"value":72.5,"system":"http://unitsofmeasure.org","code":"/min"}}"#;
    request(port, "POST", "/Observation", &[], observation);
    let by_code = request(port, "GET", "/Observation?code=http%3A%2F%2Floinc.org%7C8867-4", &[], &[]);
    let by_reference = request(port, "GET", "/Observation?patient=pt-k1", &[], &[]);
    let by_quantity = request(port, "GET", "/Observation?value-quantity=gt70", &[], &[]);
    let below = request(port, "GET", "/Observation?value-quantity=lt70", &[], &[]);
    let unknown = request(port, "GET", "/Observation?nonesuch=1", &[], &[]);
    let unsortable = request(port, "GET", "/Observation?_sort=code", &[], &[]);
    let malformed = request(port, "GET", "/Observation?date=whenever", &[], &[]);
    stop(child);

    assert_eq!(ids(&by_code.body), vec!["ob-k1".to_owned()]);
    assert_eq!(ids(&by_reference.body), vec!["ob-k1".to_owned()]);
    assert_eq!(ids(&by_quantity.body), vec!["ob-k1".to_owned()]);
    assert_eq!(total(&below.body), 0);
    assert_eq!(unknown.status, 400);
    assert_eq!(issue_code(&unknown.body), "not-supported");
    assert_eq!(unsortable.status, 400);
    assert_eq!(issue_code(&unsortable.body), "not-supported");
    assert_eq!(malformed.status, 400);
    assert_eq!(issue_code(&malformed.body), "invalid");
}

fn modes(body: &str, mode: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body).expect("bundle must be json");
    value["entry"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["search"]["mode"] == mode)
                .map(|item| item["resource"]["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn seed_advanced(port: u16) {
    let clinic = br#"{"resourceType":"Organization","id":"org-a1","name":"Mercy","active":true}"#;
    let ann = br#"{"resourceType":"Patient","id":"pt-a1","gender":"female","name":[{"family":"Sorensen"}],"managingOrganization":{"reference":"Organization/org-a1"},"identifier":[{"type":{"coding":[{"system":"urn:t","code":"MR"}]},"system":"urn:mrn","value":"12345"}]}"#;
    let bo = br#"{"resourceType":"Patient","id":"pt-a2","gender":"male","name":[{"family":"Okonkwo"}]}"#;
    let warm = br#"{"resourceType":"Observation","id":"ob-a1","status":"final","code":{"text":"probe"},"code":{"text":"Body Temperature","coding":[{"system":"urn:s","code":"vital.temperature"}]},"subject":{"reference":"Patient/pt-a1"}}"#;
    let survey = br#"{"resourceType":"Observation","id":"ob-a2","status":"registered","code":{"text":"probe"},"code":{"coding":[{"system":"urn:s","code":"survey"}]},"subject":{"reference":"Patient/pt-a2"}}"#;
    let set = br#"{"resourceType":"ValueSet","id":"vs-a1","url":"http://x/vitals","status":"active","compose":{"include":[{"system":"urn:s","concept":[{"code":"vital.temperature"}]}]}}"#;
    request(port, "POST", "/Organization", &[], clinic);
    request(port, "POST", "/Patient", &[], ann);
    request(port, "POST", "/Patient", &[], bo);
    request(port, "POST", "/Observation", &[], warm);
    request(port, "POST", "/Observation", &[], survey);
    request(port, "POST", "/ValueSet", &[], set);
}

#[test]
fn live_search_applies_every_modifier() {
    let (child, port) = spawn_server();
    seed_advanced(port);
    let exact = request(port, "GET", "/Patient?family:exact=Okonkwo", &[], &[]);
    let contains = request(port, "GET", "/Patient?family:contains=oren", &[], &[]);
    let missing = request(port, "GET", "/Patient?identifier:missing=true", &[], &[]);
    let not = request(port, "GET", "/Patient?gender:not=male", &[], &[]);
    let text = request(port, "GET", "/Observation?code:text=temperature", &[], &[]);
    let inside = request(port, "GET", "/Observation?code:in=http%3A%2F%2Fx%2Fvitals", &[], &[]);
    let outside = request(port, "GET", "/Observation?code:not-in=http%3A%2F%2Fx%2Fvitals", &[], &[]);
    let below = request(port, "GET", "/Observation?code:below=vital", &[], &[]);
    let above = request(port, "GET", "/Observation?code:above=vital.temperature.core", &[], &[]);
    let typed = request(port, "GET", "/Observation?subject:Patient=pt-a1", &[], &[]);
    let identified = request(port, "GET", "/Patient?identifier:of-type=urn:t%7CMR%7C12345", &[], &[]);
    let forbidden = request(port, "GET", "/Patient?gender:exact=male", &[], &[]);
    stop(child);

    assert_eq!(ids(&exact.body), vec!["pt-a2".to_owned()]);
    assert_eq!(ids(&contains.body), vec!["pt-a1".to_owned()]);
    assert_eq!(ids(&missing.body), vec!["pt-a2".to_owned()]);
    assert_eq!(ids(&not.body), vec!["pt-a1".to_owned()]);
    assert_eq!(ids(&text.body), vec!["ob-a1".to_owned()]);
    assert_eq!(ids(&inside.body), vec!["ob-a1".to_owned()]);
    assert_eq!(ids(&outside.body), vec!["ob-a2".to_owned()]);
    assert_eq!(ids(&below.body), vec!["ob-a1".to_owned()]);
    assert_eq!(ids(&above.body), vec!["ob-a1".to_owned()]);
    assert_eq!(ids(&typed.body), vec!["ob-a1".to_owned()]);
    assert_eq!(ids(&identified.body), vec!["pt-a1".to_owned()]);
    assert_eq!(forbidden.status, 400);
    assert_eq!(issue_code(&forbidden.body), "not-supported");
}

#[test]
fn live_search_chains_includes_and_compartments() {
    let (child, port) = spawn_server();
    seed_advanced(port);
    let chained = request(port, "GET", "/Observation?patient.gender=female", &[], &[]);
    let deep = request(port, "GET", "/Observation?patient.organization.name=Mercy", &[], &[]);
    let reverse = request(port, "GET", "/Patient?_has:Observation:patient:status=final", &[], &[]);
    let included = request(port, "GET", "/Observation?_id=ob-a1&_include=Observation:subject", &[], &[]);
    let iterated = request(
        port,
        "GET",
        "/Observation?_id=ob-a1&_include=Observation:subject&_include:iterate=Patient:organization",
        &[],
        &[],
    );
    let reverse_include = request(port, "GET", "/Patient?_id=pt-a1&_revinclude=Observation:patient", &[], &[]);
    let compartment = request(port, "GET", "/Patient/pt-a1/Observation", &[], &[]);
    let wildcard = request(port, "GET", "/Patient/pt-a1/*", &[], &[]);
    let definitions = request(port, "GET", "/CompartmentDefinition", &[], &[]);
    let one = request(port, "GET", "/CompartmentDefinition/Patient", &[], &[]);
    let outside = request(port, "GET", "/Patient/pt-a1/Organization", &[], &[]);
    stop(child);

    assert_eq!(ids(&chained.body), vec!["ob-a1".to_owned()]);
    assert_eq!(ids(&deep.body), vec!["ob-a1".to_owned()]);
    assert_eq!(ids(&reverse.body), vec!["pt-a1".to_owned()]);
    assert_eq!(modes(&included.body, "include"), vec!["pt-a1".to_owned()]);
    let mut iterated_ids = modes(&iterated.body, "include");
    iterated_ids.sort();
    assert_eq!(iterated_ids, vec!["org-a1".to_owned(), "pt-a1".to_owned()]);
    assert_eq!(modes(&reverse_include.body, "include"), vec!["ob-a1".to_owned()]);
    assert_eq!(modes(&compartment.body, "match"), vec!["ob-a1".to_owned()]);
    let mut gathered = modes(&wildcard.body, "match");
    gathered.sort();
    assert_eq!(gathered, vec!["ob-a1".to_owned(), "pt-a1".to_owned()]);
    assert_eq!(definitions.status, 200);
    assert_eq!(one.status, 200);
    assert_eq!(outside.status, 400);
    assert_eq!(issue_code(&outside.body), "not-supported");
}

#[test]
fn live_continuation_tokens_are_opaque_and_scoped() {
    let (child, port) = spawn_server();
    seed_advanced(port);
    let first = request(port, "GET", "/Patient?_count=1&_sort=_id", &[], &[]);
    let token = next_token(&first.body);
    let second = request(port, "GET", &format!("/Patient?_count=1&_sort=_id&ct={token}"), &[], &[]);
    let elsewhere = request(port, "GET", &format!("/Patient?_count=1&_sort=-_id&ct={token}"), &[], &[]);
    let edited = format!("{}{}", &token[..token.len() - 1], if token.ends_with('0') { '1' } else { '0' });
    let tampered = request(port, "GET", &format!("/Patient?_count=1&_sort=_id&ct={edited}"), &[], &[]);
    stop(child);

    assert_eq!(token.len(), 32);
    assert_eq!(ids(&first.body), vec!["pt-a1".to_owned()]);
    assert_eq!(ids(&second.body), vec!["pt-a2".to_owned()]);
    assert_eq!(elsewhere.status, 400);
    assert_eq!(issue_code(&elsewhere.body), "invalid");
    assert_eq!(tampered.status, 400);
    assert_eq!(issue_code(&tampered.body), "invalid");
}

#[test]
fn live_a_shared_key_keeps_a_token_across_a_restart() {
    let shared = [("FHIR_CONTINUATION_KEY", "a-shared-continuation-key-value")];
    let (first_child, first_port) = spawn_with(&shared);
    seed_advanced(first_port);
    let first = request(first_port, "GET", "/Patient?_count=1&_sort=_id", &[], &[]);
    let token = next_token(&first.body);
    stop(first_child);

    let (second_child, second_port) = spawn_with(&shared);
    seed_advanced(second_port);
    let carried = request(
        second_port,
        "GET",
        &format!("/Patient?_count=1&_sort=_id&ct={token}"),
        &[],
        &[],
    );
    stop(second_child);

    let (other_child, other_port) = spawn_with(&[("FHIR_CONTINUATION_KEY", "another-key-entirely")]);
    seed_advanced(other_port);
    let refused = request(
        other_port,
        "GET",
        &format!("/Patient?_count=1&_sort=_id&ct={token}"),
        &[],
        &[],
    );
    stop(other_child);

    assert_eq!(carried.status, 200, "{}", carried.body);
    assert_eq!(ids(&carried.body), vec!["pt-a2".to_owned()]);
    assert_eq!(refused.status, 400, "{}", refused.body);
}

#[test]
fn live_the_entry_limit_follows_the_connection_count() {
    let (child, port) = spawn_with(&[("FHIR_STORE_CONNECTIONS", "2")]);
    let bundle = br#"{"resourceType":"Bundle","type":"batch","entry":[
        {"request":{"method":"GET","url":"Patient/absent-one"}},
        {"request":{"method":"GET","url":"Patient/absent-two"}},
        {"request":{"method":"GET","url":"Patient/absent-three"}}
    ]}"#;
    let reply = request(port, "POST", "/", &[("Content-Type", "application/fhir+json")], bundle);
    stop(child);

    assert_eq!(reply.status, 200, "{}", reply.body);
}

fn live_definition(id: &str, code: &str) -> Vec<u8> {
    format!(
        r#"{{"resourceType":"SearchParameter","id":"{id}","name":"{code}","description":"a parameter","url":"urn:p:{code}","status":"active","code":"{code}","base":["Patient"],"type":"token","expression":"Patient.extension.valueCode"}}"#
    )
    .into_bytes()
}

fn live_banded(id: &str, code: &str) -> Vec<u8> {
    format!(
        r#"{{"resourceType":"Patient","id":"{id}","extension":[{{"url":"urn:x:band","valueCode":"{code}"}}]}}"#
    )
    .into_bytes()
}

fn live_status(body: &str, url: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body).expect("parameters must be json");
    value["parameter"]
        .as_array()
        .and_then(|entries| {
            entries.iter().find_map(|entry| {
                let parts = entry["part"].as_array()?;
                let read = |name: &str| {
                    parts
                        .iter()
                        .find(|part| part["name"] == name)
                        .and_then(|part| part["valueCode"].as_str().or_else(|| part["valueUri"].as_str()))
                        .map(str::to_owned)
                };
                (read("url").as_deref() == Some(url)).then(|| read("status"))?
            })
        })
        .unwrap_or_default()
}

#[test]
fn live_custom_parameters_register_reindex_and_answer() {
    let (child, port) = spawn_server();
    request(port, "POST", "/Patient", &[], &live_banded("pt-x1", "high"));
    request(port, "POST", "/Patient", &[], &live_banded("pt-x2", "low"));
    let created = request(port, "POST", "/SearchParameter", &[], &live_definition("sp-x1", "risk-band"));
    let awaiting = request(port, "GET", "/Patient?risk-band=high", &[], &[]);
    let supported = request(port, "GET", "/SearchParameter/$status", &[], &[]);
    let reindexed = request(port, "POST", "/SearchParameter/$reindex", &[], &[]);
    let searchable = request(port, "GET", "/SearchParameter/$status", &[], &[]);
    let found = request(port, "GET", "/Patient?risk-band=high", &[], &[]);
    let disabled = request(port, "PUT", "/SearchParameter/$status?url=urn:p:risk-band&status=disabled", &[], &[]);
    let refused = request(port, "GET", "/Patient?risk-band=high", &[], &[]);
    let unknown = request(port, "GET", "/Patient?nonesuch", &[], &[]);
    let empty = request(port, "GET", "/Patient?_id=", &[], &[]);
    stop(child);

    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(awaiting.status, 400);
    assert_eq!(issue_code(&awaiting.body), "not-supported");
    assert_eq!(live_status(&supported.body, "urn:p:risk-band"), "supported");
    assert_eq!(reindexed.status, 200, "{}", reindexed.body);
    assert_eq!(live_status(&searchable.body, "urn:p:risk-band"), "searchable");
    assert_eq!(found.status, 200, "{}", found.body);
    assert_eq!(ids(&found.body), vec!["pt-x1".to_owned()]);
    assert_eq!(live_status(&disabled.body, "urn:p:risk-band"), "pending-disable");
    assert_eq!(refused.status, 400);
    assert_eq!(unknown.status, 400);
    assert_eq!(empty.status, 400);
    assert_eq!(issue_code(&empty.body), "not-supported");
}

#[test]
fn live_search_is_confined_to_the_granted_scope() {
    let (child, port) = spawn_server();
    let patient = br#"{"resourceType":"Patient","id":"pt-g1","active":true}"#;
    let other = br#"{"resourceType":"Patient","id":"pt-g2","active":true}"#;
    let mine = br#"{"resourceType":"Observation","id":"ob-g1","status":"final","code":{"text":"probe"},"subject":{"reference":"Patient/pt-g1"}}"#;
    let theirs = br#"{"resourceType":"Observation","id":"ob-g2","status":"final","code":{"text":"probe"},"subject":{"reference":"Patient/pt-g2"}}"#;
    request(port, "POST", "/Patient", &[], patient);
    request(port, "POST", "/Patient", &[], other);
    request(port, "POST", "/Observation", &[], mine);
    request(port, "POST", "/Observation", &[], theirs);
    let scope = [("X-Scope", "compartment=Patient/pt-g1")];
    let confined = request(port, "GET", "/Observation", &scope, &[]);
    let refused = request(port, "GET", "/Observation", &[("X-Scope", "types=Patient")], &[]);
    let long = "u".repeat(600);
    let tagged = format!(
        r#"{{"resourceType":"Patient","id":"pt-g3","identifier":[{{"system":"urn:mrn","value":"{long}-a"}}]}}"#
    );
    request(port, "POST", "/Patient", &[], tagged.as_bytes());
    let exact = request(port, "GET", &format!("/Patient?identifier=urn:mrn|{long}-a"), &[], &[]);
    let miss = request(port, "GET", &format!("/Patient?identifier=urn:mrn|{long}-b"), &[], &[]);
    stop(child);

    assert_eq!(confined.status, 200, "{}", confined.body);
    assert_eq!(ids(&confined.body), vec!["ob-g1".to_owned()]);
    assert_eq!(refused.status, 403);
    assert_eq!(issue_code(&refused.body), "forbidden");
    assert_eq!(ids(&exact.body), vec!["pt-g3".to_owned()]);
    assert_eq!(total(&miss.body), serde_json::json!(0));
}

fn bundle_body(kind: &str, entries: &str) -> Vec<u8> {
    format!(r#"{{"resourceType":"Bundle","type":"{kind}","entry":[{entries}]}}"#).into_bytes()
}

fn entry(method: &str, url: &str, resource: &str) -> String {
    match resource.is_empty() {
        true => format!(r#"{{"request":{{"method":"{method}","url":"{url}"}}}}"#),
        false => format!(
            r#"{{"resource":{resource},"request":{{"method":"{method}","url":"{url}"}}}}"#
        ),
    }
}

#[test]
fn bundles_are_processed_over_http() {
    let (child, port) = spawn_server();
    let headers = [("Content-Type", "application/fhir+json")];

    let applied = request(
        port,
        "POST",
        "/",
        &headers,
        &bundle_body(
            "transaction",
            &format!(
                "{},{}",
                entry("POST", "Patient", r#"{"resourceType":"Patient","id":"bn-1","active":true}"#),
                entry("POST", "Patient", r#"{"resourceType":"Patient","id":"bn-2","active":false}"#)
            ),
        ),
    );
    assert_eq!(applied.status, 200, "transaction failed: {}", applied.body);
    let value: serde_json::Value = serde_json::from_str(&applied.body).unwrap();
    assert_eq!(value["type"], "transaction-response");
    assert_eq!(value["entry"][0]["response"]["status"], "201 Created");
    assert_eq!(request(port, "GET", "/Patient/bn-2", &[], &[]).status, 200);

    let refused = request(
        port,
        "POST",
        "/",
        &headers,
        &bundle_body(
            "transaction",
            &format!(
                "{},{}",
                entry("POST", "Patient", r#"{"resourceType":"Patient","id":"bn-3","active":true}"#),
                entry("POST", "Nonesuch", r#"{"resourceType":"Nonesuch","id":"bn-4"}"#)
            ),
        ),
    );
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(!issue_code(&refused.body).is_empty());
    assert_eq!(request(port, "GET", "/Patient/bn-3", &[], &[]).status, 404);

    let mixed = request(
        port,
        "POST",
        "/",
        &headers,
        &bundle_body(
            "batch",
            &format!(
                "{},{},{}",
                entry("POST", "Patient", r#"{"resourceType":"Patient","id":"bn-5","active":true}"#),
                entry("POST", "Nonesuch", r#"{"resourceType":"Nonesuch","id":"bn-6"}"#),
                entry("GET", "Patient/bn-1", "")
            ),
        ),
    );
    assert_eq!(mixed.status, 200, "batch failed: {}", mixed.body);
    let value: serde_json::Value = serde_json::from_str(&mixed.body).unwrap();
    assert_eq!(value["type"], "batch-response");
    assert_eq!(value["entry"][0]["response"]["status"], "201 Created");
    assert_eq!(value["entry"][1]["outcome"]["resourceType"], "OperationOutcome");
    assert_eq!(value["entry"][2]["resource"]["id"], "bn-1");
    assert_eq!(request(port, "GET", "/Patient/bn-5", &[], &[]).status, 200);

    stop(child);
}

#[test]
fn a_submitted_job_is_polled_to_completion_over_http() {
    let (child, port) = spawn_server();

    let created = request(port, "POST", "/Patient", &[], &patient("jb-1", true));
    assert_eq!(created.status, 201, "create failed: {}", created.body);

    let submitted = request(port, "POST", "/$export", &[], br#"{"types":["Patient"]}"#);
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    let location = header(&submitted, "content-location").to_owned();
    assert!(location.contains("/_jobs/"), "{location}");
    assert_eq!(header(&submitted, "retry-after"), "1");
    let path = location
        .split_once("/_jobs/")
        .map(|(_, id)| format!("/_jobs/{id}"))
        .expect("a status location carries an id");

    let mut polled = request(port, "GET", &path, &[], &[]);
    for _ in 0..100 {
        if polled.status != 202 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        polled = request(port, "GET", &path, &[], &[]);
    }
    stop(child);

    assert_eq!(polled.status, 200, "poll failed: {}", polled.body);
    let value: serde_json::Value = serde_json::from_str(&polled.body).expect("a manifest is json");
    assert_eq!(value["state"], "completed");
    assert_eq!(value["kind"], "export");
    assert_eq!(value["outcome"]["handled"], 1);
}

#[test]
fn a_submitted_job_is_cancelled_over_http() {
    let (child, port) = spawn_server();

    let submitted = request(port, "POST", "/$import", &[], br#"{"resources":[]}"#);
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    let location = header(&submitted, "content-location").to_owned();
    let path = location
        .split_once("/_jobs/")
        .map(|(_, id)| format!("/_jobs/{id}"))
        .expect("a status location carries an id");

    let cancelled = request(port, "DELETE", &path, &[], &[]);
    let polled = request(port, "GET", &path, &[], &[]);
    stop(child);

    assert!(
        cancelled.status == 202 || cancelled.status == 409,
        "cancel answered {}",
        cancelled.status
    );
    assert!(
        polled.status == 404 || polled.status == 200,
        "poll answered {}",
        polled.status
    );
}

fn settled(port: u16, path: &str) -> Reply {
    let mut polled = request(port, "GET", path, &[], &[]);
    for _ in 0..100 {
        if polled.status != 202 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        polled = request(port, "GET", path, &[], &[]);
    }
    polled
}

fn job_path(reply: &Reply) -> String {
    header(reply, "content-location")
        .split_once("/_jobs/")
        .map(|(_, id)| format!("/_jobs/{id}"))
        .expect("a status location carries an id")
}

#[test]
fn newline_delimited_rows_are_imported_and_a_repeat_adds_no_version() {
    let (child, port) = spawn_server();

    let supplied = format!(
        "{}\n{{ this is not a resource\n{}\n",
        String::from_utf8(patient("nd-1", true)).unwrap(),
        String::from_utf8(patient("nd-2", false)).unwrap(),
    );
    let submitted = request(
        port,
        "POST",
        "/$import",
        &[("Content-Type", "application/fhir+ndjson")],
        supplied.as_bytes(),
    );
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    let first = settled(port, &job_path(&submitted));
    assert_eq!(first.status, 200, "poll failed: {}", first.body);
    let loaded: serde_json::Value = serde_json::from_str(&first.body).expect("a manifest is json");
    assert_eq!(loaded["outcome"]["handled"], 2);
    assert_eq!(loaded["outcome"]["unchanged"], 0);
    let failures = loaded["outcome"]["failures"]
        .as_array()
        .expect("failures are listed");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].as_str().unwrap().starts_with("row 1"), "{failures:?}");

    let stored = request(port, "GET", "/Patient/nd-1", &[], &[]);
    assert_eq!(stored.status, 200, "read failed: {}", stored.body);
    assert_eq!(header(&stored, "etag"), "W/\"1\"");

    let again = request(
        port,
        "POST",
        "/$import",
        &[("Content-Type", "application/fhir+ndjson")],
        supplied.as_bytes(),
    );
    let repeated = settled(port, &job_path(&again));
    let held: serde_json::Value = serde_json::from_str(&repeated.body).expect("a manifest is json");
    let unmoved = request(port, "GET", "/Patient/nd-1", &[], &[]);
    stop(child);

    assert_eq!(held["outcome"]["handled"], 0);
    assert_eq!(held["outcome"]["unchanged"], 2);
    assert_eq!(header(&unmoved, "etag"), "W/\"1\"");
}

#[test]
fn an_export_manifest_details_the_request_it_answered() {
    let (child, port) = spawn_server();

    let created = request(port, "POST", "/Patient", &[], &patient("xp-1", true));
    assert_eq!(created.status, 201, "create failed: {}", created.body);

    let submitted = request(port, "GET", "/Patient/$export?_type=Patient", &[], &[]);
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    let done = settled(port, &job_path(&submitted));
    assert_eq!(done.status, 200, "poll failed: {}", done.body);
    let manifest: serde_json::Value = serde_json::from_str(&done.body).expect("a manifest is json");
    let file = manifest["output"][0]["url"]
        .as_str()
        .expect("an output file is listed")
        .split_once("/_jobs/")
        .map(|(_, tail)| format!("/_jobs/{tail}"))
        .expect("an output url carries a job");
    let rows = request(port, "GET", &file, &[], &[]);
    stop(child);

    assert_eq!(manifest["request"]["scope"], "patient");
    assert_eq!(manifest["request"]["_type"][0], "Patient");
    assert_eq!(manifest["progress"]["done"], 1);
    assert_eq!(manifest["progress"]["total"], 1);
    assert!(manifest["transactionTime"].is_string(), "{manifest}");
    assert_eq!(rows.status, 200, "output failed: {}", rows.body);
    assert!(rows.body.contains("xp-1"), "{}", rows.body);
}

#[test]
fn a_bulk_delete_caps_what_it_removes_and_then_clears_the_marked() {
    let (child, port) = spawn_server();

    request(port, "POST", "/Patient", &[], &patient("bd-1", true));
    request(port, "POST", "/Patient", &[], &patient("bd-2", false));

    let capped = request(port, "POST", "/Patient/$bulk-delete?_maxCount=1", &[], &[]);
    assert_eq!(capped.status, 202, "submit failed: {}", capped.body);
    let first = settled(port, &job_path(&capped));
    assert_eq!(first.status, 200, "poll failed: {}", first.body);
    let report: serde_json::Value = serde_json::from_str(&first.body).expect("a manifest is json");
    assert_eq!(report["outcome"]["handled"], 1);
    assert_eq!(report["outcome"]["Patient"]["deleted"], 1);

    let rest = request(port, "POST", "/Patient/$bulk-delete", &[], &[]);
    let second = settled(port, &job_path(&rest));
    let cleared: serde_json::Value =
        serde_json::from_str(&second.body).expect("a manifest is json");
    assert_eq!(cleared["outcome"]["handled"], 1);

    let marked = request(port, "GET", "/Patient/bd-1", &[], &[]);
    let purged = request(
        port,
        "POST",
        "/Patient/$bulk-delete-soft-deleted?_hardDelete=true",
        &[],
        &[],
    );
    let third = settled(port, &job_path(&purged));
    let gone: serde_json::Value = serde_json::from_str(&third.body).expect("a manifest is json");
    let after = request(port, "GET", "/Patient/bd-1", &[], &[]);
    let history = request(port, "GET", "/Patient/bd-1/_history/1", &[], &[]);
    stop(child);

    assert_eq!(marked.status, 410, "a marked resource reads as gone");
    assert_eq!(gone["outcome"]["handled"], 2);
    assert_eq!(after.status, 404, "read said {}", after.body);
    assert_eq!(history.status, 404, "history said {}", history.body);
}

#[test]
fn a_bulk_update_patches_a_type_and_reports_its_progress() {
    let (child, port) = spawn_server();

    request(port, "POST", "/Patient", &[], &patient("bu-1", true));
    request(port, "POST", "/Patient", &[], &patient("bu-2", true));

    let submitted = request(
        port,
        "POST",
        "/Patient/$bulk-update",
        &[("Content-Type", "application/json-patch+json")],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    );
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    let done = settled(port, &job_path(&submitted));
    assert_eq!(done.status, 200, "poll failed: {}", done.body);
    let manifest: serde_json::Value = serde_json::from_str(&done.body).expect("a manifest is json");

    let repeated = request(
        port,
        "POST",
        "/Patient/$bulk-update",
        &[("Content-Type", "application/json-patch+json")],
        br#"[{"op":"replace","path":"/active","value":false}]"#,
    );
    let again = settled(port, &job_path(&repeated));
    let unmoved: serde_json::Value = serde_json::from_str(&again.body).expect("a manifest is json");
    let read = request(port, "GET", "/Patient/bu-1", &[], &[]);
    stop(child);

    assert_eq!(manifest["outcome"]["handled"], 2);
    assert_eq!(manifest["outcome"]["Patient"]["patched"], 2);
    assert_eq!(manifest["progress"]["done"], 1);
    assert_eq!(manifest["progress"]["total"], 1);
    assert_eq!(unmoved["outcome"]["handled"], 0);
    assert_eq!(unmoved["outcome"]["unchanged"], 2);
    assert_eq!(read.status, 200, "read failed: {}", read.body);
    assert_eq!(header(&read, "etag"), "W/\"2\"");
    assert!(read.body.contains("\"active\":false"), "{}", read.body);
}

#[test]
fn a_reindex_of_one_resource_makes_it_findable_again() {
    let (child, port) = spawn_server();

    request(port, "POST", "/Patient", &[], &live_banded("rx-1", "high"));
    request(port, "POST", "/Patient", &[], &live_banded("rx-2", "high"));
    request(
        port,
        "POST",
        "/SearchParameter",
        &[],
        &live_definition("sp-r1", "risk-band"),
    );
    let backfilled = request(port, "POST", "/SearchParameter/$reindex", &[], &[]);
    assert_eq!(backfilled.status, 200, "backfill failed: {}", backfilled.body);

    let moved = request(port, "PUT", "/Patient/rx-1", &[], &live_banded("rx-1", "low"));
    assert_eq!(moved.status, 200, "update failed: {}", moved.body);
    let stale = request(port, "GET", "/Patient?risk-band=low", &[], &[]);

    let submitted = request(port, "POST", "/Patient/rx-1/$reindex", &[], &[]);
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    let done = settled(port, &job_path(&submitted));
    assert_eq!(done.status, 200, "poll failed: {}", done.body);
    let manifest: serde_json::Value = serde_json::from_str(&done.body).expect("a manifest is json");
    let after = request(port, "GET", "/Patient?risk-band=low", &[], &[]);
    let others = request(port, "GET", "/Patient?risk-band=high", &[], &[]);
    stop(child);

    assert!(stale.body.contains("\"total\":0"), "{}", stale.body);
    assert_eq!(manifest["request"]["_resource"][0], "Patient/rx-1");
    assert_eq!(manifest["outcome"]["handled"], 1);
    assert_eq!(manifest["outcome"]["units"], 1);
    assert_eq!(ids(&after.body), vec!["rx-1".to_owned()]);
    assert_eq!(ids(&others.body), vec!["rx-2".to_owned()]);
}

fn seed_operations(port: u16) {
    let patient = br#"{"resourceType":"Patient","id":"pt-o1","gender":"female","identifier":[{"system":"urn:mrn","value":"90210"}],"birthDate":"1980-04-01"}"#;
    let other = br#"{"resourceType":"Patient","id":"pt-o2","gender":"male"}"#;
    let observation = br#"{"resourceType":"Observation","id":"ob-o1","status":"final","code":{"text":"probe"},"code":{"coding":[{"system":"urn:cs","code":"leaf"}]},"subject":{"reference":"Patient/pt-o1"}}"#;
    let document = br#"{"resourceType":"DocumentReference","id":"dr-o1","status":"current","type":{"coding":[{"system":"urn:doc","code":"note"}]},"subject":{"reference":"Patient/pt-o1"},"date":"2026-03-01T00:00:00Z","content":[{"attachment":{"url":"urn:doc:body"}}]}"#;
    let system = br#"{"resourceType":"CodeSystem","id":"cs-o1","url":"urn:cs","version":"1.0","status":"active","content":"complete","concept":[{"code":"top","display":"Top","concept":[{"code":"mid","display":"Middle","concept":[{"code":"leaf","display":"Leaf"}]}]}]}"#;
    let set = br#"{"resourceType":"ValueSet","id":"vs-o1","url":"urn:vs","status":"active","compose":{"include":[{"system":"urn:cs"}]}}"#;
    request(port, "POST", "/Patient", &[], patient);
    request(port, "POST", "/Patient", &[], other);
    request(port, "POST", "/Observation", &[], observation);
    request(port, "POST", "/DocumentReference", &[], document);
    request(port, "POST", "/CodeSystem", &[], system);
    request(port, "POST", "/ValueSet", &[], set);
}

#[test]
fn live_extended_operations_answer() {
    let (child, port) = spawn_server();
    seed_operations(port);
    let conversion = br#"{"resourceType":"Parameters","parameter":[{"name":"inputData","valueString":"PID|1||pt-c1||Ann^Bea||19800401|female"},{"name":"inputDataType","valueString":"hl7v2"},{"name":"templateCollectionReference","valueString":"urn:template-collection:default"},{"name":"rootTemplate","valueString":"Patient"}]}"#;
    let converted = request(port, "POST", "/$convert-data", &[], conversion);
    let validated = request(
        port,
        "POST",
        "/Patient/$validate",
        &[],
        br#"{"resourceType":"Patient","id":"pt-v1","active":true}"#,
    );
    let unstored = request(port, "GET", "/Patient/pt-v1", &[], &[]);
    let everything = request(port, "GET", "/Patient/pt-o1/$everything", &[], &[]);
    let matched = request(
        port,
        "POST",
        "/Patient/$member-match",
        &[],
        br#"{"resourceType":"Parameters","parameter":[{"name":"MemberPatient","resource":{"resourceType":"Patient","identifier":[{"system":"urn:mrn","value":"90210"}]}}]}"#,
    );
    let unmatched = request(
        port,
        "POST",
        "/Patient/$member-match",
        &[],
        br#"{"resourceType":"Parameters","parameter":[{"name":"MemberPatient","resource":{"resourceType":"Patient","identifier":[{"system":"urn:mrn","value":"nonesuch"}]}}]}"#,
    );
    let includes = request(
        port,
        "GET",
        "/Observation/$includes?_include=Observation:subject",
        &[],
        &[],
    );
    let docref = request(port, "GET", "/DocumentReference/$docref?patient=pt-o1", &[], &[]);
    let posted = request(
        port,
        "POST",
        "/DocumentReference/$docref",
        &[],
        br#"{"resourceType":"Parameters","parameter":[{"name":"patient","valueString":"pt-o1"}]}"#,
    );
    let expanded = request(port, "GET", "/ValueSet/$expand?url=urn:vs&excludeNested=true", &[], &[]);
    let unknown = request(port, "GET", "/ValueSet/$expand?url=urn:none", &[], &[]);
    let below = request(port, "GET", "/Observation?code:below=urn:cs%7Cmid", &[], &[]);
    stop(child);

    assert_eq!(converted.status, 200);
    let rendered: serde_json::Value = serde_json::from_str(&converted.body).expect("json");
    assert_eq!(rendered["resourceType"], "Patient");
    assert_eq!(rendered["id"], "pt-c1");
    assert_eq!(validated.status, 200);
    assert_eq!(issue_code(&validated.body), "informational");
    assert_eq!(unstored.status, 404);
    assert_eq!(everything.status, 200);
    let gathered = ids(&everything.body);
    assert!(gathered.contains(&"pt-o1".to_owned()), "{gathered:?}");
    assert!(gathered.contains(&"ob-o1".to_owned()), "{gathered:?}");
    assert_eq!(matched.status, 200);
    let member: serde_json::Value = serde_json::from_str(&matched.body).expect("json");
    assert_eq!(member["parameter"][0]["valueIdentifier"]["value"], "90210");
    assert_eq!(unmatched.status, 422);
    assert_eq!(issue_code(&unmatched.body), "business-rule");
    assert_eq!(includes.status, 200);
    assert_eq!(ids(&includes.body), vec!["pt-o1".to_owned()]);
    assert_eq!(docref.status, 200);
    assert_eq!(ids(&docref.body), vec!["dr-o1".to_owned()]);
    assert_eq!(ids(&posted.body), ids(&docref.body));
    assert_eq!(expanded.status, 200);
    let expansion: serde_json::Value = serde_json::from_str(&expanded.body).expect("json");
    assert_eq!(expansion["expansion"]["total"], 3);
    assert_eq!(expansion["expansion"]["contains"][2]["code"], "leaf");
    assert_eq!(unknown.status, 404);
    assert_eq!(ids(&below.body), vec!["ob-o1".to_owned()]);
}

fn spawn_authorized() -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "memory")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
        .env("FHIR_AUTH_ISSUER", "https://issuer.example.org")
        .env("FHIR_AUTH_AUTHORIZE", "https://issuer.example.org/authorize")
        .env("FHIR_AUTH_TOKEN", "https://issuer.example.org/token")
        .env("FHIR_AUTH_SCOPES", "system/*.read,system/*.write")
        .env_remove("FHIR_DATABASE_URL")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn binary");
    let stdout = child.stdout.take().expect("missing stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("failed to read the announced address");
    match line.trim().rsplit_once(':').and_then(|(_, port)| port.parse().ok()) {
        Some(port) => (child, port),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("server did not announce an address, said {line:?}");
        }
    }
}

#[test]
fn live_conformance_is_served_from_the_running_routes() {
    let (child, port) = spawn_server();
    let statement = request(port, "GET", "/metadata", &[], &[]);
    let definitions = request(port, "GET", "/OperationDefinition", &[], &[]);
    let export = request(port, "GET", "/OperationDefinition/export", &[], &[]);
    let unknown = request(port, "GET", "/OperationDefinition/nonesuch", &[], &[]);
    let discovery = request(port, "GET", "/.well-known/smart-configuration", &[], &[]);
    let versions = request(port, "GET", "/$versions", &[], &[]);
    stop(child);

    assert_eq!(statement.status, 200, "metadata failed: {}", statement.body);
    let value: serde_json::Value = serde_json::from_str(&statement.body).unwrap();
    assert_eq!(value["resourceType"], "CapabilityStatement");
    assert_eq!(value["fhirVersion"], "4.0.1");
    assert!(value["rest"][0]["security"].is_null());
    let patient = value["rest"][0]["resource"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["type"] == "Patient")
        .expect("Patient must be listed")
        .clone();
    assert!(patient["searchParam"]
        .as_array()
        .unwrap()
        .iter()
        .any(|param| param["name"] == "birthdate"));

    assert_eq!(definitions.status, 200);
    assert_eq!(export.status, 200);
    let exported: serde_json::Value = serde_json::from_str(&export.body).unwrap();
    assert_eq!(exported["resourceType"], "OperationDefinition");
    assert_eq!(exported["code"], "export");
    assert_eq!(unknown.status, 404);
    assert_eq!(discovery.status, 404);

    assert_eq!(versions.status, 200, "versions failed: {}", versions.body);
    let reported: serde_json::Value = serde_json::from_str(&versions.body).unwrap();
    assert_eq!(reported["resourceType"], "Parameters");
    let default = reported["parameter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "default")
        .expect("a default version must be reported")
        .clone();
    assert_eq!(default["valueCode"], "4.0.1");
}

#[test]
fn live_discovery_matches_the_configured_authorization() {
    let (child, port) = spawn_authorized();
    let discovery = request(port, "GET", "/.well-known/smart-configuration", &[], &[]);
    let statement = request(port, "GET", "/metadata", &[], &[]);
    stop(child);

    assert_eq!(discovery.status, 200, "discovery failed: {}", discovery.body);
    let document: serde_json::Value = serde_json::from_str(&discovery.body).unwrap();
    assert_eq!(document["issuer"], "https://issuer.example.org");
    assert_eq!(document["token_endpoint"], "https://issuer.example.org/token");
    assert_eq!(document["scopes_supported"][0], "system/*.read");

    let value: serde_json::Value = serde_json::from_str(&statement.body).unwrap();
    assert_eq!(
        value["rest"][0]["security"]["service"][0]["coding"][0]["code"],
        "SMART-on-FHIR"
    );
}

const SMOKE_ISSUER: &str = "https://issuer.example.org";

fn smoke_signing() -> &'static fhir_core::security::fixture::Issuer {
    use std::sync::OnceLock;
    static HELD: OnceLock<fhir_core::security::fixture::Issuer> = OnceLock::new();
    HELD.get_or_init(|| fhir_core::security::fixture::Issuer::generate("one"))
}

fn key_document() -> String {
    smoke_signing().keys().to_string()
}

fn smoke_claims(scopes: &str, life: i64) -> serde_json::Value {
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64 + life)
        .unwrap_or(0);
    serde_json::json!({
        "iss": SMOKE_ISSUER,
        "sub": "practitioner-1",
        "scope": scopes,
        "exp": expiry,
    })
}

fn smoke_token(scopes: &str) -> String {
    smoke_signing().mint(&smoke_claims(scopes, 300))
}

fn spawn_enforcing() -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "memory")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
        .env("FHIR_AUTH_ISSUER", SMOKE_ISSUER)
        .env("FHIR_AUTH_AUTHORIZE", "https://issuer.example.org/authorize")
        .env("FHIR_AUTH_TOKEN", "https://issuer.example.org/token")
        .env("FHIR_AUTH_INTROSPECT", "https://issuer.example.org/introspect")
        .env("FHIR_AUTH_SCOPES", "system/*.read,system/*.write")
        .env("FHIR_AUTH_KEYS", key_document())
        .env_remove("FHIR_DATABASE_URL")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn binary");
    let stdout = child.stdout.take().expect("missing stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("failed to read the announced address");
    match line.trim().rsplit_once(':').and_then(|(_, port)| port.parse().ok()) {
        Some(port) => (child, port),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("server did not announce an address, said {line:?}");
        }
    }
}

#[test]
fn live_requests_are_authorized_scoped_and_recorded() {
    let (child, port) = spawn_enforcing();
    let write = format!("Bearer {}", smoke_token("system/Patient.read system/Patient.write"));
    let read_all = format!("Bearer {}", smoke_token("system/*.read"));
    let patient = br#"{"resourceType":"Patient","id":"pt-sec","active":true,"name":[{"family":"Stone"}]}"#;

    let anonymous = request(port, "GET", "/Patient/pt-sec", &[], &[]);
    let created = request(port, "POST", "/Patient", &[("Authorization", write.as_str())], patient);
    let read = request(port, "GET", "/Patient/pt-sec", &[("Authorization", write.as_str())], &[]);
    let outside = request(port, "GET", "/Observation", &[("Authorization", write.as_str())], &[]);
    let introspected = request(
        port,
        "POST",
        "/_introspect",
        &[("Authorization", write.as_str())],
        format!("token={}", smoke_token("system/Patient.read")).as_bytes(),
    );
    let trail = request(port, "GET", "/AuditEvent", &[("Authorization", read_all.as_str())], &[]);
    stop(child);

    assert_eq!(anonymous.status, 401, "{}", anonymous.body);
    assert_eq!(issue_code(&anonymous.body), "login");
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(outside.status, 403, "{}", outside.body);
    assert_eq!(issue_code(&outside.body), "forbidden");

    assert_eq!(introspected.status, 200, "{}", introspected.body);
    let reported: serde_json::Value = serde_json::from_str(&introspected.body).expect("json");
    assert_eq!(reported["active"], true);
    assert_eq!(reported["sub"], "practitioner-1");
    assert_eq!(reported["scope"], "system/Patient.read");

    assert_eq!(trail.status, 200, "{}", trail.body);
    let recorded: serde_json::Value = serde_json::from_str(&trail.body).expect("json");
    let entries = recorded["entry"].as_array().cloned().unwrap_or_default();
    assert!(!entries.is_empty(), "{}", trail.body);
    let actions: Vec<String> = entries
        .iter()
        .map(|entry| entry["resource"]["type"]["code"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(actions.contains(&"write".to_owned()), "{actions:?}");
    assert!(actions.contains(&"read".to_owned()), "{actions:?}");
    assert!(!trail.body.contains("Stone"), "{}", trail.body);
}

fn smoke_refusable() -> Vec<(&'static str, String)> {
    use fhir_core::security::fixture::Issuer;
    let elsewhere = Issuer::generate("one");
    let claims = smoke_claims("system/*.read system/*.write", 300);
    let symmetric = serde_json::json!({"alg": "HS256", "kid": "one"});
    let input = Issuer::input(&symmetric, &claims);
    let forged = jsonwebtoken::crypto::sign(
        input.as_bytes(),
        &jsonwebtoken::EncodingKey::from_secret(smoke_signing().material().as_bytes()),
        jsonwebtoken::Algorithm::HS256,
    )
    .expect("the library signs");
    vec![
        ("signed by another key", elsewhere.mint(&claims)),
        (
            "carrying no signature",
            format!(
                "{}.",
                Issuer::input(&serde_json::json!({"alg": "ES256", "kid": "one"}), &claims)
            ),
        ),
        ("naming a symmetric algorithm", format!("{input}.{forged}")),
        (
            "naming an algorithm the key does not verify",
            smoke_signing()
                .minted_under(&serde_json::json!({"alg": "ES384", "kid": "one"}), &claims),
        ),
        ("expired", smoke_signing().mint(&smoke_claims("system/*.read", -60))),
    ]
}

#[test]
fn live_a_token_the_published_key_does_not_verify_is_refused() {
    let (child, port) = spawn_enforcing();
    let offered = smoke_refusable();
    let replies: Vec<(String, u16, String)> = offered
        .iter()
        .map(|(reason, token)| {
            let carried = format!("Bearer {token}");
            let reply = request(
                port,
                "POST",
                "/Patient",
                &[("Authorization", carried.as_str())],
                br#"{"resourceType":"Patient","id":"pt-forged","active":true}"#,
            );
            ((*reason).to_owned(), reply.status, reply.body)
        })
        .collect();
    let honest = format!("Bearer {}", smoke_token("system/*.read"));
    let absent = request(port, "GET", "/Patient/pt-forged", &[("Authorization", honest.as_str())], &[]);
    stop(child);

    for (reason, status, body) in &replies {
        assert_eq!(*status, 401, "{reason}: {body}");
        assert_eq!(issue_code(body), "login", "{reason}: {body}");
    }
    assert_eq!(absent.status, 404, "{}", absent.body);
}

const CREDENTIAL: &str = "a-live-reader-credential";

#[test]
fn an_unconfigured_instance_does_not_serve_its_measurements() {
    let (child, port) = spawn_server();
    let read = request(port, "GET", "/Patient/pt-confidential-77", &[], &[]);
    let bare = request(port, "GET", "/_metrics", &[], &[]);
    let offered = request(
        port,
        "GET",
        "/_metrics",
        &[("Authorization", "Bearer a-live-reader-credential")],
        &[],
    );
    stop(child);

    assert_eq!(read.status, 404, "{}", read.body);
    assert_eq!(bare.status, 404, "{}", bare.body);
    assert_eq!(offered.status, 404, "{}", offered.body);
    for reply in [&bare, &offered] {
        assert!(!reply.body.contains("fhir_operation_total"), "{}", reply.body);
        assert!(!reply.body.contains("duration_ms"), "{}", reply.body);
    }
}

#[test]
fn a_guarded_instance_serves_measurements_only_to_its_reader() {
    let (child, port) = spawn_with(&[("FHIR_METRICS_CREDENTIAL", CREDENTIAL)]);
    let held = format!("Bearer {CREDENTIAL}");
    let statement = request(port, "GET", "/metadata", &[], &[]);
    let read = request(port, "GET", "/Patient/pt-confidential-77", &[], &[]);
    let bare = request(port, "GET", "/_metrics", &[], &[]);
    let wrong = request(
        port,
        "GET",
        "/_metrics",
        &[("Authorization", "Bearer another-reader-credential")],
        &[],
    );
    let served = request(port, "GET", "/_metrics", &[("Authorization", held.as_str())], &[]);
    stop(child);

    assert_eq!(statement.status, 200);
    assert_eq!(read.status, 404);
    assert_eq!(bare.status, 403, "{}", bare.body);
    assert_eq!(wrong.status, 403, "{}", wrong.body);
    assert_eq!(served.status, 200, "{}", served.body);
    assert!(header(&served, "content-type").starts_with("text/plain"));
    assert!(served
        .body
        .contains("fhir_operation_total{operation=\"conformance\",outcome=\"success\"} 1"));
    assert!(served
        .body
        .contains("fhir_operation_total{operation=\"read\",outcome=\"client_fault\"} 1"));
    assert!(served.body.contains("fhir_operation_duration_ms_bucket"));
    for secret in ["pt-confidential-77", CREDENTIAL] {
        assert!(!served.body.contains(secret), "{secret} was served");
    }
}

#[test]
fn every_live_answer_carries_an_identifier_of_the_shape_we_issue() {
    let (child, port) = spawn_server();
    let plain = request(port, "GET", "/metadata", &[], &[]);
    let offered = request(
        port,
        "GET",
        "/metadata",
        &[("X-Correlation-Id", "patient-smith-4711")],
        &[],
    );
    stop(child);

    for reply in [&plain, &offered] {
        let carried = header(reply, "x-correlation-id");
        assert_eq!(carried.len(), 32, "{carried:?}");
        assert!(carried.chars().all(|held| held.is_ascii_hexdigit()));
    }
    assert_ne!(header(&offered, "x-correlation-id"), "patient-smith-4711");
}
