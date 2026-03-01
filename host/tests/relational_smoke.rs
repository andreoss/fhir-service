use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const REQUIRED: &str =
    "the relational engine is required and none answered; start the services named in compose.yaml";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
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

fn spawn_server(namespace: &str) -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "relational")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
        .env("FHIR_DATABASE_URL", url())
        .env("FHIR_SCHEMA", namespace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn binary");
    let stdout = child.stdout.take().expect("missing stdout");
    let mut line = String::new();
    let read = BufReader::new(stdout)
        .read_line(&mut line)
        .unwrap_or_default();
    let port = line
        .trim()
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok());
    match (read, port) {
        (_, Some(port)) => (child, port),
        _ => {
            let mut told = String::new();
            if let Some(stderr) = child.stderr.take() {
                let _ = BufReader::new(stderr).read_line(&mut told);
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("{REQUIRED}: {}", told.trim());
        }
    }
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn drop_schema(namespace: &str) {
    let statement = format!("drop schema if exists {namespace} cascade");
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
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
    let headers = head
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body,
    }
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
    let (child, port) = spawn_server(&namespace);

    let health = request(port, "GET", "/health", &[], &[]);
    assert_eq!(health.status, 200, "{}", health.body);
    let reported = json(&health.body);
    assert_eq!(reported["status"], "ok");
    let mut named: Vec<String> = reported["dependencies"]
        .as_array()
        .expect("dependencies are listed")
        .iter()
        .map(|held| {
            held["name"]
                .as_str()
                .expect("a dependency is named")
                .to_owned()
        })
        .collect();
    named.sort();
    assert_eq!(
        named,
        vec!["outputs", "queue", "store"],
        "each engine this instance opened is reported"
    );

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

fn bundle_body(kind: &str, entries: &str) -> Vec<u8> {
    format!(r#"{{"resourceType":"Bundle","type":"{kind}","entry":[{entries}]}}"#).into_bytes()
}

fn entry(method: &str, url: &str, resource: &str) -> String {
    match resource.is_empty() {
        true => format!(r#"{{"request":{{"method":"{method}","url":"{url}"}}}}"#),
        false => {
            format!(r#"{{"resource":{resource},"request":{{"method":"{method}","url":"{url}"}}}}"#)
        }
    }
}

fn patient_text(id: &str, family: &str) -> String {
    String::from_utf8(patient(id, family, true)).expect("the fixture is text")
}

#[test]
fn bundles_are_atomic_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);
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
                entry("POST", "Patient", &patient_text("bx1", "Stone")),
                entry("POST", "Patient", &patient_text("bx2", "Rivers"))
            ),
        ),
    );
    assert_eq!(applied.status, 200, "{}", applied.body);
    assert_eq!(json(&applied.body)["type"], "transaction-response");
    assert_eq!(request(port, "GET", "/Patient/bx1", &[], &[]).status, 200);
    assert_eq!(request(port, "GET", "/Patient/bx2", &[], &[]).status, 200);

    let refused = request(
        port,
        "POST",
        "/",
        &headers,
        &bundle_body(
            "transaction",
            &format!(
                "{},{}",
                entry("POST", "Patient", &patient_text("bx3", "Vale")),
                entry("POST", "Patient", &patient_text("bx1", "Stone"))
            ),
        ),
    );
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(request(port, "GET", "/Patient/bx3", &[], &[]).status, 404);

    let mixed = request(
        port,
        "POST",
        "/",
        &headers,
        &bundle_body(
            "batch",
            &format!(
                "{},{},{}",
                entry("POST", "Patient", &patient_text("bx4", "Marsh")),
                entry("POST", "Patient", &patient_text("bx1", "Stone")),
                entry("GET", "Patient?name=stone", "")
            ),
        ),
    );
    assert_eq!(mixed.status, 200, "{}", mixed.body);
    let value = json(&mixed.body);
    assert_eq!(value["entry"][0]["response"]["status"], "201 Created");
    assert_eq!(
        value["entry"][1]["outcome"]["resourceType"],
        "OperationOutcome"
    );
    assert_eq!(value["entry"][2]["resource"]["total"], 1);

    stop(child);
    drop_schema(&namespace);
}

#[test]
fn a_transaction_resolves_its_placeholders_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);
    let headers = [("Content-Type", "application/fhir+json")];
    let place = "urn:uuid:5f3ad0c4-7c2b-4f2e-9a5d-6d5c8e1f4b21";
    let observation = format!(
        r#"{{"resourceType":"Observation","id":"ob1","status":"final","code":{{"coding":[{{"system":"urn:s","code":"c1"}}]}},"subject":{{"reference":"{place}"}}}}"#
    );
    let applied = request(
        port,
        "POST",
        "/",
        &headers,
        &bundle_body(
            "transaction",
            &format!(
                "{},{}",
                placed_entry("POST", "Patient", &patient_text("bp1", "Quarry"), place),
                entry("POST", "Observation", &observation)
            ),
        ),
    );
    assert_eq!(applied.status, 200, "{}", applied.body);
    assert_eq!(
        json(&applied.body)["entry"][1]["resource"]["subject"]["reference"],
        "Patient/bp1",
        "{}",
        applied.body
    );
    let stored = request(port, "GET", "/Observation/ob1", &[], &[]);
    assert_eq!(stored.status, 200, "{}", stored.body);
    assert_eq!(json(&stored.body)["subject"]["reference"], "Patient/bp1");
    let document = request(port, "GET", "/Patient/bp1/$everything", &[], &[]);
    assert_eq!(document.status, 200, "{}", document.body);

    stop(child);
    drop_schema(&namespace);
}

fn placed_entry(method: &str, url: &str, resource: &str, full_url: &str) -> String {
    format!(
        r#"{{"fullUrl":"{full_url}","resource":{resource},"request":{{"method":"{method}","url":"{url}"}}}}"#
    )
}

fn header<'a>(reply: &'a Reply, name: &str) -> &'a str {
    reply
        .headers
        .iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

#[test]
fn a_job_runs_to_completion_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    let created = request(
        port,
        "POST",
        "/Patient",
        &[],
        &patient("jr-1", "Stone", true),
    );
    assert_eq!(created.status, 201, "create failed: {}", created.body);

    let submitted = request(port, "POST", "/$export", &[], br#"{"types":["Patient"]}"#);
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    assert_eq!(header(&submitted, "retry-after"), "1");
    let path = header(&submitted, "content-location")
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
    drop_schema(&namespace);

    assert_eq!(polled.status, 200, "poll failed: {}", polled.body);
    let manifest = json(&polled.body);
    assert_eq!(manifest["state"], "completed");
    assert_eq!(manifest["kind"], "export");
    assert_eq!(manifest["outcome"]["handled"], 1);
}

#[test]
fn a_return_preference_is_honoured_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    let minimal = request(
        port,
        "POST",
        "/Patient",
        &[
            ("Content-Type", "application/fhir+json"),
            ("Prefer", "return=minimal"),
        ],
        &patient("rt-1", "Stone", true),
    );
    assert_eq!(minimal.status, 201, "{}", minimal.body);
    assert!(minimal.body.is_empty(), "{}", minimal.body);

    let outcome = request(
        port,
        "PUT",
        "/Patient/rt-1",
        &[
            ("Content-Type", "application/fhir+json"),
            ("Prefer", "return=OperationOutcome"),
        ],
        &patient("rt-1", "Stone", false),
    );
    assert_eq!(outcome.status, 200, "{}", outcome.body);
    assert_eq!(json(&outcome.body)["resourceType"], "OperationOutcome");
    assert_eq!(json(&outcome.body)["issue"][0]["severity"], "information");

    let again = request(
        port,
        "PUT",
        "/Patient/rt-1",
        &[
            ("Content-Type", "application/fhir+json"),
            ("Prefer", "return=representation"),
        ],
        &patient("rt-1", "Stone", true),
    );
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(json(&again.body)["resourceType"], "Patient");

    let bundle = request(
        port,
        "POST",
        "/",
        &[
            ("Content-Type", "application/fhir+json"),
            ("Prefer", "return=minimal"),
        ],
        &bundle_body(
            "transaction",
            &format!(
                "{},{}",
                entry("POST", "Patient", &patient_text("rt-2", "Rivers")),
                entry("POST", "Patient", &patient_text("rt-3", "Marsh"))
            ),
        ),
    );
    assert_eq!(bundle.status, 200, "{}", bundle.body);
    let value = json(&bundle.body);
    for index in 0..2 {
        assert_eq!(value["entry"][index]["response"]["status"], "201 Created");
        assert!(
            value["entry"][index].get("resource").is_none(),
            "{}",
            value["entry"][index]
        );
    }

    stop(child);
    drop_schema(&namespace);
}

#[test]
fn a_search_asked_to_answer_later_is_answered_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    let created = request(
        port,
        "POST",
        "/Patient",
        &[],
        &patient("da-1", "River", true),
    );
    assert_eq!(created.status, 201, "create failed: {}", created.body);

    let deferred = request(
        port,
        "GET",
        "/Patient?family=River",
        &[("Prefer", "respond-async")],
        &[],
    );
    assert_eq!(deferred.status, 202, "kick-off failed: {}", deferred.body);
    assert_eq!(header(&deferred, "retry-after"), "1");
    let path = header(&deferred, "content-location")
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
    drop_schema(&namespace);

    assert_eq!(polled.status, 200, "poll failed: {}", polled.body);
    let answered = json(&polled.body);
    assert_eq!(answered["resourceType"], "Bundle");
    assert_eq!(answered["type"], "batch-response");
    assert_eq!(answered["entry"][0]["response"]["status"], "200 OK");
    let carried = &answered["entry"][0]["resource"];
    assert_eq!(carried["type"], "searchset");
    assert_eq!(carried["entry"][0]["resource"]["id"], "da-1");
}
