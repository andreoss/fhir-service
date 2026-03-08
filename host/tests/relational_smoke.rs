use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const REQUIRED: &str =
    "the relational engine is required and none answered; start the services named in compose.yaml";

const POLL_WAIT: Duration = Duration::from_millis(1_100);

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
    spawn_version(namespace, "R4")
}

fn spawn_version(namespace: &str, version: &str) -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "relational")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", version)
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
        std::thread::sleep(POLL_WAIT);
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
fn a_client_that_polls_too_often_is_asked_to_wait_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    for _ in 0..40 {
        let queued = request(port, "POST", "/$import", &[], br#"{"resources":[]}"#);
        assert_eq!(queued.status, 202, "submit failed: {}", queued.body);
    }
    let submitted = request(port, "POST", "/$export", &[], br#"{"types":["Patient"]}"#);
    assert_eq!(submitted.status, 202, "submit failed: {}", submitted.body);
    let path = header(&submitted, "content-location")
        .split_once("/_jobs/")
        .map(|(_, id)| format!("/_jobs/{id}"))
        .expect("a status location carries an id");

    let first = request(port, "GET", &path, &[], &[]);
    let soon = request(port, "GET", &path, &[], &[]);
    std::thread::sleep(POLL_WAIT);
    let later = request(port, "GET", &path, &[], &[]);
    stop(child);
    drop_schema(&namespace);

    assert_eq!(
        first.status, 202,
        "the job is still in progress: {}",
        first.body
    );
    assert_eq!(
        soon.status, 429,
        "a second query inside the wait is refused: {}",
        soon.body
    );
    assert_eq!(header(&soon, "retry-after"), "1");
    assert_eq!(json(&soon.body)["issue"][0]["code"], "throttled");
    assert_ne!(later.status, 429, "the wait was kept: {}", later.body);
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
        std::thread::sleep(POLL_WAIT);
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

#[test]
fn a_read_of_what_has_not_changed_answers_not_modified_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    let created = request(
        port,
        "PUT",
        "/Patient/cr-1",
        &[("Content-Type", "application/fhir+json")],
        &patient("cr-1", "Stone", true),
    );
    let etag = header(&created, "etag").to_owned();
    let written = header(&created, "last-modified").to_owned();
    let held = request(
        port,
        "GET",
        "/Patient/cr-1",
        &[("If-None-Match", &etag)],
        &[],
    );
    let any = request(port, "GET", "/Patient/cr-1", &[("If-None-Match", "*")], &[]);
    let since = request(
        port,
        "GET",
        "/Patient/cr-1",
        &[("If-Modified-Since", &written)],
        &[],
    );
    let stale = request(
        port,
        "GET",
        "/Patient/cr-1",
        &[("If-None-Match", "W/\"9\"")],
        &[],
    );
    let early = request(
        port,
        "GET",
        "/Patient/cr-1",
        &[("If-Modified-Since", "Sun, 06 Sep 2026 03:59:59 GMT")],
        &[],
    );
    let whole = request(port, "GET", "/Patient/cr-1", &[], &[]);
    stop(child);
    drop_schema(&namespace);

    assert_eq!(created.status, 201, "create failed: {}", created.body);
    assert_eq!(etag, "W/\"1\"", "{}", created.body);
    assert!(!written.is_empty(), "{}", created.body);
    assert_eq!(held.status, 304, "the version is held: {}", held.body);
    assert!(held.body.is_empty(), "no body: {}", held.body);
    assert_eq!(header(&held, "etag"), etag, "{}", held.body);
    assert_eq!(header(&held, "last-modified"), written, "{}", held.body);
    assert_eq!(header(&held, "cache-control"), "no-cache", "{}", held.body);
    assert!(
        header(&held, "content-location").contains("/Patient/cr-1"),
        "{}",
        held.body
    );
    assert_eq!(any.status, 304, "any version is held: {}", any.body);
    assert_eq!(since.status, 304, "nothing changed since: {}", since.body);
    assert_eq!(stale.status, 200, "a stale version: {}", stale.body);
    assert_eq!(json(&stale.body)["resourceType"], "Patient");
    assert_eq!(early.status, 200, "an earlier date: {}", early.body);
    assert_eq!(whole.status, 200, "no precondition: {}", whole.body);
}

#[test]
fn a_search_names_the_common_parameters_of_the_release_over_the_relational_backend() {
    fn carried(id: &str, language: &str, source: &str) -> Vec<u8> {
        format!(
            r#"{{"resourceType":"Patient","id":"{id}","language":"{language}","meta":{{"source":"{source}"}},"name":[{{"family":"Stone"}}]}}"#
        )
        .into_bytes()
    }

    let namespace = schema();
    let (child, port) = spawn_version(&namespace, "R5");
    for (id, language, source) in [
        ("cm-1", "en", "http://example.org/records/one"),
        ("cm-2", "fr", "http://example.org/records/two"),
    ] {
        let reply = request(
            port,
            "PUT",
            &format!("/Patient/{id}"),
            &[("Content-Type", "application/fhir+json")],
            &carried(id, language, source),
        );
        assert_eq!(reply.status, 201, "create failed: {}", reply.body);
    }

    let by_language = request(port, "GET", "/Patient?_language=en", &[], &[]);
    let both = request(port, "GET", "/Patient?_language=fr,en", &[], &[]);
    let no_language = request(port, "GET", "/Patient?_language=de", &[], &[]);
    let by_source = request(
        port,
        "GET",
        "/Patient?_source=http%3A%2F%2Fexample.org%2Frecords%2Fone",
        &[],
        &[],
    );
    let no_source = request(
        port,
        "GET",
        "/Patient?_source=http%3A%2F%2Fexample.org%2Frecords%2Fthree",
        &[],
        &[],
    );
    stop(child);
    let earlier = schema();
    let (child, port) = spawn_version(&earlier, "R4");
    let refused = request(port, "GET", "/Patient?_language=en", &[], &[]);
    let answered = request(
        port,
        "GET",
        "/Patient?_source=http%3A%2F%2Fexample.org%2Frecords%2Fone",
        &[],
        &[],
    );
    stop(child);
    drop_schema(&namespace);
    drop_schema(&earlier);

    assert_eq!(
        by_language.status, 200,
        "the language is answered: {}",
        by_language.body
    );
    assert_eq!(json(&by_language.body)["total"], 1, "{}", by_language.body);
    assert_eq!(
        json(&by_language.body)["entry"][0]["resource"]["id"],
        "cm-1",
        "{}",
        by_language.body
    );
    assert_eq!(json(&both.body)["total"], 2, "{}", both.body);
    assert_eq!(json(&no_language.body)["total"], 0, "{}", no_language.body);
    assert_eq!(
        json(&by_source.body)["entry"][0]["resource"]["id"],
        "cm-1",
        "{}",
        by_source.body
    );
    assert_eq!(json(&no_source.body)["total"], 0, "{}", no_source.body);
    assert_eq!(
        refused.status, 400,
        "a release that names no language refuses one: {}",
        refused.body
    );
    assert!(
        refused.body.contains("_language"),
        "the refusal names the parameter: {}",
        refused.body
    );
    assert_eq!(
        answered.status, 200,
        "the source is answered where the release names it: {}",
        answered.body
    );
}

#[test]
fn a_search_names_the_members_of_a_collection_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_version(&namespace, "R5");
    for (path, body) in [
        ("/Patient/101", r#"{"resourceType":"Patient","id":"101"}"#),
        ("/Patient/102", r#"{"resourceType":"Patient","id":"102"}"#),
        (
            "/Group/grp-1",
            r#"{"resourceType":"Group","id":"grp-1","type":"person","membership":"enumerated","member":[{"entity":{"reference":"Patient/101"}},{"entity":{"reference":"Patient/102"},"inactive":true}]}"#,
        ),
        (
            "/List/lst-1",
            r#"{"resourceType":"List","id":"lst-1","status":"current","mode":"working","entry":[{"item":{"reference":"Patient/102"}}]}"#,
        ),
        (
            "/Observation/ob-1",
            r#"{"resourceType":"Observation","id":"ob-1","status":"final","code":{"text":"weight"},"subject":{"reference":"Patient/101"}}"#,
        ),
        (
            "/Observation/ob-2",
            r#"{"resourceType":"Observation","id":"ob-2","status":"final","code":{"text":"weight"},"subject":{"reference":"Patient/102"}}"#,
        ),
    ] {
        let reply = request(
            port,
            "PUT",
            path,
            &[("Content-Type", "application/fhir+json")],
            body.as_bytes(),
        );
        assert_eq!(reply.status, 201, "create failed {path}: {}", reply.body);
    }

    let by_group = request(port, "GET", "/Patient?_in=Group/grp-1", &[], &[]);
    let inverted = request(port, "GET", "/Patient?_in:not=Group/grp-1", &[], &[]);
    let by_list = request(port, "GET", "/Patient?_in=List/lst-1", &[], &[]);
    let absent = request(port, "GET", "/Patient?_in=Group/grp-2", &[], &[]);
    let chained = request(
        port,
        "GET",
        "/Observation?subject._in=Group/grp-1",
        &[],
        &[],
    );
    let chained_by_id = request(port, "GET", "/Observation?subject._id=101", &[], &[]);
    stop(child);
    drop_schema(&namespace);

    assert_eq!(
        by_group.status, 200,
        "the members are answered: {}",
        by_group.body
    );
    assert_eq!(
        json(&chained_by_id.body)["total"],
        1,
        "a forward chain finds nothing: {}",
        chained_by_id.body
    );
    assert_eq!(json(&by_group.body)["total"], 1, "{}", by_group.body);
    assert_eq!(
        json(&by_group.body)["entry"][0]["resource"]["id"],
        "101",
        "{}",
        by_group.body
    );
    assert_eq!(json(&inverted.body)["total"], 1, "{}", inverted.body);
    assert_eq!(
        json(&inverted.body)["entry"][0]["resource"]["id"],
        "102",
        "{}",
        inverted.body
    );
    assert_eq!(json(&by_list.body)["total"], 1, "{}", by_list.body);
    assert_eq!(
        json(&by_list.body)["entry"][0]["resource"]["id"],
        "102",
        "{}",
        by_list.body
    );
    assert_eq!(json(&absent.body)["total"], 0, "{}", absent.body);
    assert_eq!(
        chained.status, 200,
        "the chained form is answered: {}",
        chained.body
    );
    assert_eq!(json(&chained.body)["total"], 1, "{}", chained.body);
    assert_eq!(
        json(&chained.body)["entry"][0]["resource"]["id"],
        "ob-1",
        "{}",
        chained.body
    );
}

#[test]
fn a_write_that_carries_a_provenance_records_it_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    let carried = serde_json::json!({
        "resourceType": "Provenance",
        "recorded": "2026-09-06T04:00:00Z",
        "agent": [{"who": {"display": "a clinician"}}]
    })
    .to_string();

    let created = request(
        port,
        "POST",
        "/Patient",
        &[
            ("Content-Type", "application/fhir+json"),
            ("X-Provenance", &carried),
        ],
        &patient("pv-1", "Stone", true),
    );

    let refused = request(
        port,
        "POST",
        "/Patient",
        &[
            ("Content-Type", "application/fhir+json"),
            ("X-Provenance", "{\"resourceType\":\"Patient\"}"),
        ],
        &patient("pv-2", "Stone", true),
    );

    let held = request(
        port,
        "GET",
        "/Provenance",
        &[("Accept", "application/fhir+json")],
        b"",
    );

    let absent = request(
        port,
        "GET",
        "/Patient/pv-2",
        &[("Accept", "application/fhir+json")],
        b"",
    );

    stop(child);
    drop_schema(&namespace);

    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert_eq!(absent.status, 404, "{}", absent.body);
    assert_eq!(held.status, 200, "{}", held.body);
    assert_eq!(json(&held.body)["total"], 1, "{}", held.body);
    assert_eq!(
        json(&held.body)["entry"][0]["resource"]["target"][0]["reference"],
        "Patient/pv-1/_history/1",
        "{}",
        held.body
    );
}

#[test]
fn everything_narrows_by_the_clinical_window_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    let json_header = [("Content-Type", "application/fhir+json")];
    request(
        port,
        "PUT",
        "/Patient/ev-1",
        &json_header,
        &patient("ev-1", "Stone", true),
    );
    for (id, effective) in [("ev-old", "2020-01-01"), ("ev-new", "2024-06-01")] {
        let body = serde_json::json!({
            "resourceType": "Observation",
            "id": id,
            "status": "final",
            "code": {"text": "probe"},
            "effectiveDateTime": effective,
            "subject": {"reference": "Patient/ev-1"}
        })
        .to_string()
        .into_bytes();
        request(
            port,
            "PUT",
            &format!("/Observation/{id}"),
            &json_header,
            &body,
        );
    }

    let whole = request(port, "GET", "/Patient/ev-1/$everything", &[], b"");
    let recent = request(
        port,
        "GET",
        "/Patient/ev-1/$everything?start=2023-01-01",
        &[],
        b"",
    );
    let refused = request(
        port,
        "GET",
        "/Patient/ev-1/$everything?_till=2023-01-01",
        &[],
        b"",
    );

    stop(child);
    drop_schema(&namespace);

    assert_eq!(whole.status, 200, "{}", whole.body);
    assert_eq!(json(&whole.body)["total"], 3, "{}", whole.body);
    assert_eq!(recent.status, 200, "{}", recent.body);
    assert_eq!(
        json(&recent.body)["total"],
        2,
        "the patient carries no clinical date and is gathered regardless: {}",
        recent.body
    );
    let named: Vec<String> = json(&recent.body)["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["resource"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(named.contains(&"ev-new".to_owned()), "{named:?}");
    assert!(!named.contains(&"ev-old".to_owned()), "{named:?}");
    assert_eq!(refused.status, 400, "{}", refused.body);
}

#[test]
fn a_lenient_search_is_answered_over_the_relational_backend() {
    let namespace = schema();
    let (child, port) = spawn_server(&namespace);

    request(
        port,
        "PUT",
        "/Patient/ln-1",
        &[("Content-Type", "application/fhir+json")],
        &patient("ln-1", "Stone", true),
    );

    let strict = request(port, "GET", "/Patient?nonesuch=x", &[], b"");
    let lenient = request(
        port,
        "GET",
        "/Patient?nonesuch=x&active=true",
        &[("Prefer", "handling=lenient")],
        b"",
    );
    let posted = request(
        port,
        "POST",
        "/Patient/_search",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        b"active=true",
    );

    stop(child);
    drop_schema(&namespace);

    assert_eq!(strict.status, 400, "{}", strict.body);
    assert_eq!(lenient.status, 200, "{}", lenient.body);
    assert_eq!(json(&lenient.body)["total"], 1, "{}", lenient.body);
    let told = json(&lenient.body)["entry"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["search"]["mode"] == "outcome");
    assert!(
        told,
        "an outcome entry names what was ignored: {}",
        lenient.body
    );
    let link = json(&lenient.body)["link"][0]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!link.contains("nonesuch"), "{link}");
    assert_eq!(posted.status, 200, "{}", posted.body);
    assert_eq!(json(&posted.body)["total"], 1, "{}", posted.body);
}
