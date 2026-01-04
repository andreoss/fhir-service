use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn spawn_server() -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "memory")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
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
    let port = line
        .trim()
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok());
    match port {
        Some(port) => (child, port),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("server did not announce an address, said {line:?}");
        }
    }
}


fn stop(mut child: Child) {
    child.kill().expect("failed to kill server");
    child.wait().expect("failed to reap server");
}

fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("failed to connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("set read timeout");
    let mut stream = stream;
    let mut head = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\n");
    if !body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(body);
    stream.write_all(&bytes).expect("failed to write request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("failed to read response");
    parse_response(&response)
}

fn parse_response(text: &str) -> Reply {
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().expect("missing response head");
    let body = parts.next().unwrap_or("").to_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("missing status line");
    let status = status_line.split_whitespace().nth(1).expect("missing status code").parse().expect("bad status code");
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').expect("malformed header");
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    Reply { status, headers, body }
}

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
