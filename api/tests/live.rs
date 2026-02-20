use fhir_adapter_memory::MemoryStore;
use fhir_api::{Authorization, Dependency, HeldKeys, Service};
use fhir_core::security::bearer::KeySet;
use fhir_core::security::fixture::Issuer;
use fhir_core::{FhirInstant, FhirVersion};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const ISSUER: &str = "https://issuer.example.org";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn header<'a>(reply: &'a Reply, name: &str) -> &'a str {
    reply
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn signing() -> &'static Issuer {
    static HELD: OnceLock<Issuer> = OnceLock::new();
    HELD.get_or_init(|| Issuer::generate("live"))
}

fn token(scopes: &str) -> String {
    signing().mint(&json!({
        "iss": ISSUER,
        "sub": "practitioner-live",
        "scope": scopes,
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 300,
    }))
}

fn store() -> MemoryStore {
    MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }))
}

fn dependencies() -> Vec<Dependency> {
    vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }]
}

fn open() -> Service {
    Service::new(Arc::new(store()), FhirVersion::R4, dependencies())
}

fn guarded() -> Service {
    let keys = KeySet::parse(&signing().keys()).expect("a published key set");
    Service::new(Arc::new(store()), FhirVersion::R4, dependencies())
        .with_authorization(Authorization::new(
            ISSUER,
            "https://issuer.example.org/a",
            "https://issuer.example.org/t",
        ))
        .enforcing(Arc::new(HeldKeys::new(keys)))
        .expect("an authorization is configured")
}

struct Running {
    address: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    served: Option<tokio::task::JoinHandle<Result<(), fhir_core::Error>>>,
}

impl Running {
    async fn started(app: Service) -> Running {
        let bound = app
            .bind("127.0.0.1:0".parse().expect("a loopback address"))
            .await
            .expect("an ephemeral port binds");
        let address = bound.local_addr().expect("the bound address is announced");
        assert_ne!(address.port(), 0, "the announced port is the assigned one");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let served = tokio::spawn(async move {
            bound
                .serve_until(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Running {
            address,
            stop: Some(stop),
            served: Some(served),
        }
    }

    async fn exchange(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Reply {
        let address = self.address;
        let method = method.to_owned();
        let path = path.to_owned();
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let body = body.to_vec();
        tokio::task::spawn_blocking(move || spoken(address, &method, &path, &headers, &body))
            .await
            .expect("the exchange completes")
    }

    async fn stopped(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(served) = self.served.take() {
            let _ = served.await;
        }
    }
}

fn spoken(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Reply {
    let mut stream = TcpStream::connect(address).expect("the announced address accepts");
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("a read timeout is set");
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .expect("a write timeout is set");
    let mut head = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n");
    if !body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    stream.write_all(&bytes).expect("the request is written");
    stream.flush().expect("the request is flushed");
    let mut answered = Vec::new();
    stream
        .read_to_end(&mut answered)
        .expect("the answer is read within the timeout");
    parsed(&String::from_utf8_lossy(&answered))
}

fn parsed(text: &str) -> Reply {
    let (head, body) = text.split_once("\r\n\r\n").expect("an answer has a head");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("an answer has a status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("a status line names a code")
        .parse()
        .expect("a status code is a number");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_owned(),
    }
}

fn patient(id: &str, active: bool) -> Vec<u8> {
    format!(r#"{{"resourceType":"Patient","id":"{id}","active":{active}}}"#).into_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn live_an_interaction_chain_is_served_over_a_socket() {
    let running = Running::started(open()).await;

    let created = running
        .exchange("POST", "/Patient", &[], &patient("lv-1", true))
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(header(&created, "etag"), "W/\"1\"");
    assert_eq!(header(&created, "content-type"), "application/fhir+json");
    assert!(
        header(&created, "location").ends_with("/Patient/lv-1/_history/1"),
        "{}",
        header(&created, "location")
    );

    let read = running.exchange("GET", "/Patient/lv-1", &[], &[]).await;
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(header(&read, "etag"), "W/\"1\"");

    let updated = running
        .exchange(
            "PUT",
            "/Patient/lv-1",
            &[("if-match", "W/\"1\"")],
            &patient("lv-1", false),
        )
        .await;
    assert_eq!(updated.status, 200, "{}", updated.body);
    assert_eq!(header(&updated, "etag"), "W/\"2\"");

    let first = running
        .exchange("GET", "/Patient/lv-1/_history/1", &[], &[])
        .await;
    assert_eq!(first.status, 200, "{}", first.body);
    assert!(first.body.contains("\"active\":true"), "{}", first.body);

    let removed = running.exchange("DELETE", "/Patient/lv-1", &[], &[]).await;
    assert_eq!(removed.status, 204, "{}", removed.body);
    let gone = running.exchange("GET", "/Patient/lv-1", &[], &[]).await;
    assert_eq!(gone.status, 410, "{}", gone.body);

    running.stopped().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_the_statuses_the_specification_names_are_the_statuses_on_the_wire() {
    let running = Running::started(open()).await;
    running
        .exchange("POST", "/Patient", &[], &patient("lv-2", true))
        .await;

    let created_by_update = running
        .exchange("PUT", "/Patient/lv-none", &[], &patient("lv-none", true))
        .await;
    assert_eq!(created_by_update.status, 201, "{}", created_by_update.body);
    assert!(header(&created_by_update, "location").ends_with("/Patient/lv-none/_history/1"));

    let stale = running
        .exchange(
            "PUT",
            "/Patient/lv-2",
            &[("if-match", "W/\"7\"")],
            &patient("lv-2", false),
        )
        .await;
    assert_eq!(stale.status, 412, "{}", stale.body);

    let unmatched = running
        .exchange("DELETE", "/Patient?_id=lv-absent", &[], &[])
        .await;
    assert_eq!(unmatched.status, 204, "{}", unmatched.body);

    let unrenderable = running
        .exchange("GET", "/Patient?_format=xml", &[], &[])
        .await;
    assert_eq!(unrenderable.status, 406, "{}", unrenderable.body);

    let unknown = running.exchange("GET", "/Patient/lv-absent", &[], &[]).await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);

    running.stopped().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_a_grant_both_grants_and_withholds_over_a_socket() {
    let running = Running::started(guarded()).await;
    let write = format!("Bearer {}", token("system/Patient.write"));
    let read = format!("Bearer {}", token("system/Patient.read"));

    let anonymous = running
        .exchange("POST", "/Patient", &[], &patient("lv-3", true))
        .await;
    assert_eq!(anonymous.status, 401, "{}", anonymous.body);

    let created = running
        .exchange(
            "POST",
            "/Patient",
            &[("authorization", write.as_str())],
            &patient("lv-3", true),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.body);

    let granted = running
        .exchange(
            "GET",
            "/Patient/lv-3",
            &[("authorization", read.as_str())],
            &[],
        )
        .await;
    assert_eq!(granted.status, 200, "{}", granted.body);

    let withheld = running
        .exchange(
            "POST",
            "/Patient",
            &[("authorization", read.as_str())],
            &patient("lv-4", true),
        )
        .await;
    assert_eq!(withheld.status, 403, "{}", withheld.body);
    let outcome: Value = serde_json::from_str(&withheld.body).expect("an outcome");
    assert_eq!(outcome["issue"][0]["code"], "forbidden");

    let elsewhere = running
        .exchange(
            "GET",
            "/Observation",
            &[("authorization", read.as_str())],
            &[],
        )
        .await;
    assert_eq!(elsewhere.status, 403, "{}", elsewhere.body);

    let never_written = running
        .exchange(
            "GET",
            "/Patient/lv-4",
            &[("authorization", read.as_str())],
            &[],
        )
        .await;
    assert_eq!(never_written.status, 404, "{}", never_written.body);

    running.stopped().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_conformance_is_served_without_a_token() {
    let running = Running::started(guarded()).await;
    for path in ["/metadata", "/health", "/.well-known/smart-configuration"] {
        let reply = running.exchange("GET", path, &[], &[]).await;
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
        assert!(
            serde_json::from_str::<Value>(&reply.body).is_ok(),
            "{path} answered {}",
            reply.body
        );
    }
    running.stopped().await;
}
