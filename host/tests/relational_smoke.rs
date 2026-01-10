use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const SKIPPED: &str = "skipped: the relational engine is not available";

struct Reply {
    status: u16,
    body: String,
}

fn url() -> String {
    std::env::var("FHIR_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://fhir:fhir@127.0.0.1:5432/fhir".to_owned())
}

fn schema() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!("t_smoke_{stamp}")
}

fn spawn_server(namespace: &str) -> Option<(Child, u16)> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "relational")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
        .env("FHIR_DATABASE_URL", url())
        .env("FHIR_SCHEMA", namespace)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn binary");
    let stdout = child.stdout.take().expect("missing stdout");
    let mut line = String::new();
    let read = BufReader::new(stdout).read_line(&mut line).unwrap_or_default();
    let port = line
        .trim()
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok());
    match (read, port) {
        (_, Some(port)) => Some((child, port)),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("{SKIPPED}");
            None
        }
    }
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn drop_schema(namespace: &str) {
    let statement = format!("drop schema if exists {namespace} cascade");
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    runtime.block_on(async {
        if let Ok(pool) = sqlx::PgPool::connect(&url()).await {
            let _ = sqlx::raw_sql(&statement).execute(&pool).await;
        }
    });
}

fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("failed to connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");
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
    let mut parts = response.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default().to_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_default();
    Reply { status, body }
}

fn patient(id: &str, family: &str, active: bool) -> Vec<u8> {
    format!(
        r#"{{"resourceType":"Patient","id":"{id}","active":{active},"name":[{{"family":"{family}"}}]}}"#
    )
    .into_bytes()
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("the body is json")
}

#[test]
fn the_service_serves_every_interaction_over_the_relational_backend() {
    let namespace = schema();
    let Some((child, port)) = spawn_server(&namespace) else { return };

    let health = request(port, "GET", "/health", &[], &[]);
    assert_eq!(health.status, 200, "{}", health.body);
    assert_eq!(json(&health.body)["status"], "ok");

    let created = request(
        port,
        "POST",
        "/Patient",
        &[("Content-Type", "application/fhir+json")],
        &patient("sm1", "Stone", true),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(json(&created.body)["meta"]["versionId"], "1");

    let read = request(port, "GET", "/Patient/sm1", &[], &[]);
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(json(&read.body)["name"][0]["family"], "Stone");

    let updated = request(
        port,
        "PUT",
        "/Patient/sm1",
        &[("Content-Type", "application/fhir+json")],
        &patient("sm1", "Rivers", true),
    );
    assert_eq!(updated.status, 200, "{}", updated.body);
    assert_eq!(json(&updated.body)["meta"]["versionId"], "2");

    let old = request(port, "GET", "/Patient/sm1/_history/1", &[], &[]);
    assert_eq!(old.status, 200, "{}", old.body);
    assert_eq!(json(&old.body)["name"][0]["family"], "Stone");

    request(
        port,
        "POST",
        "/Patient",
        &[("Content-Type", "application/fhir+json")],
        &patient("sm2", "Fields", false),
    );

    let found = request(port, "GET", "/Patient?name=riv", &[], &[]);
    assert_eq!(found.status, 200, "{}", found.body);
    let bundle = json(&found.body);
    assert_eq!(bundle["total"], 1);
    assert_eq!(bundle["entry"][0]["resource"]["id"], "sm1");

    let sorted = request(port, "GET", "/Patient?_sort=name&_count=1", &[], &[]);
    assert_eq!(sorted.status, 200, "{}", sorted.body);
    assert_eq!(json(&sorted.body)["entry"][0]["resource"]["id"], "sm2");

    let history = request(port, "GET", "/Patient/sm1/_history", &[], &[]);
    assert_eq!(history.status, 200, "{}", history.body);
    assert_eq!(json(&history.body)["total"], 2);

    let refused = request(port, "GET", "/Patient?nonesuch=1", &[], &[]);
    assert_eq!(refused.status, 400, "{}", refused.body);

    let deleted = request(port, "DELETE", "/Patient/sm1", &[], &[]);
    assert_eq!(deleted.status, 204, "{}", deleted.body);
    let gone = request(port, "GET", "/Patient/sm1", &[], &[]);
    assert_eq!(gone.status, 410, "{}", gone.body);

    let after = request(port, "GET", "/Patient?name=riv", &[], &[]);
    assert_eq!(json(&after.body)["total"], 0);

    let missing = request(port, "GET", "/Patient/nobody", &[], &[]);
    assert_eq!(missing.status, 404, "{}", missing.body);

    stop(child);
    drop_schema(&namespace);
}
