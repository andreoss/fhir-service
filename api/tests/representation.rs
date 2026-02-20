use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceExt;

const READ_TIMEOUT: Duration = Duration::from_secs(10);

struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-06T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
}

async fn request(
    app: &Service,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_vec())).unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn header<'a>(reply: &'a Reply, name: &str) -> &'a str {
    reply
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

fn patient(id: &str) -> Vec<u8> {
    format!(r#"{{"resourceType":"Patient","id":"{id}","active":true}}"#).into_bytes()
}

async fn seeded() -> Service {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("rp-1")).await;
    app
}

#[tokio::test]
async fn an_absent_accept_header_answers_the_default_representation() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient/rp-1", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), "application/fhir+json");
    assert!(!reply.body.contains('\n'), "{}", reply.body);
}

#[tokio::test]
async fn an_accept_header_names_the_representation_of_the_answer() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/Patient/rp-1",
        &[("accept", "application/json")],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), "application/json");
}

#[tokio::test]
async fn a_wildcard_accept_header_answers_the_default_representation() {
    let app = seeded().await;
    for accept in ["*/*", "application/*", "text/*"] {
        let reply = request(&app, "GET", "/Patient/rp-1", &[("accept", accept)], &[]).await;
        assert_eq!(reply.status, StatusCode::OK, "{accept}: {}", reply.body);
        assert_eq!(
            header(&reply, "content-type"),
            "application/fhir+json",
            "{accept}"
        );
    }
}

#[tokio::test]
async fn the_best_supported_media_type_is_chosen_by_quality() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/Patient/rp-1",
        &[(
            "accept",
            "application/json;q=1, application/fhir+json;q=0.5",
        )],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), "application/json");
}

#[tokio::test]
async fn a_media_type_refused_by_quality_is_not_answered() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/Patient/rp-1",
        &[("accept", "text/html, application/fhir+json;q=0")],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_ACCEPTABLE, "{}", reply.body);
}

#[tokio::test]
async fn an_unsupported_accept_header_is_refused() {
    let app = seeded().await;
    for accept in ["text/html", "application/fhir+turtle", "text/plain"] {
        let reply = request(&app, "GET", "/Patient/rp-1", &[("accept", accept)], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_ACCEPTABLE,
            "{accept}: {}",
            reply.body
        );
        assert_eq!(
            header(&reply, "content-type"),
            "application/fhir+json",
            "{accept}"
        );
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["resourceType"], "OperationOutcome", "{accept}");
        assert_eq!(value["issue"][0]["code"], "not-supported", "{accept}");
    }
}

#[tokio::test]
async fn an_unsupported_format_parameter_is_refused_on_every_interaction() {
    let app = seeded().await;
    for uri in [
        "/Patient/rp-1?_format=yaml",
        "/Patient?_format=yaml",
        "/metadata?_format=yaml",
        "/Patient/rp-1/_history?_format=yaml",
    ] {
        let reply = request(&app, "GET", uri, &[], &[]).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_ACCEPTABLE,
            "{uri}: {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn a_refused_representation_has_no_effect_on_the_store() {
    let app = seeded().await;
    let reply = request(
        &app,
        "PUT",
        "/Patient/rp-refused?_format=yaml",
        &[],
        &patient("rp-refused"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_ACCEPTABLE, "{}", reply.body);
    let after = request(&app, "GET", "/Patient/rp-refused", &[], &[]).await;
    assert_eq!(after.status, StatusCode::NOT_FOUND, "{}", after.body);
}

#[tokio::test]
async fn pretty_is_honoured_on_a_resource_and_on_an_outcome() {
    let app = seeded().await;
    let pretty = request(&app, "GET", "/Patient/rp-1?_pretty=true", &[], &[]).await;
    assert_eq!(pretty.status, StatusCode::OK, "{}", pretty.body);
    assert!(pretty.body.contains('\n'), "{}", pretty.body);
    assert!(pretty.body.contains("\n  \""), "{}", pretty.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&pretty.body).unwrap()["id"],
        "rp-1"
    );

    let refused = request(&app, "GET", "/Patient/rp-absent?_pretty=true", &[], &[]).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
    assert!(refused.body.contains('\n'), "{}", refused.body);
    let value: serde_json::Value = serde_json::from_str(&refused.body).unwrap();
    assert_eq!(value["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn pretty_false_keeps_the_compact_form() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient/rp-1?_pretty=false", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(!reply.body.contains('\n'), "{}", reply.body);
}

#[tokio::test]
async fn an_unreadable_pretty_parameter_is_refused() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient/rp-1?_pretty=perhaps", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
}

#[tokio::test]
async fn a_bundle_and_a_search_answer_in_the_asked_representation() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient?_format=json&_pretty=true", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), "application/fhir+json");
    let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["resourceType"], "Bundle");
    assert!(reply.body.contains('\n'), "{}", reply.body);
}

#[tokio::test]
async fn a_response_that_carries_no_json_body_is_left_alone() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/health?_pretty=true",
        &[("accept", "application/fhir+json")],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), "application/json");
}

struct Running {
    address: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    served: Option<tokio::task::JoinHandle<Result<(), fhir_core::Error>>>,
}

impl Running {
    async fn started(app: Service) -> Running {
        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().expect("loopback"))
            .await
            .expect("an ephemeral port binds");
        let address = listener
            .local_addr()
            .expect("the bound address is announced");
        assert_ne!(address.port(), 0, "the announced port is the assigned one");
        let router = app.router();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let served = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await
                .map_err(|error| fhir_core::Error::Internal(error.to_string()))
        });
        Running {
            address,
            stop: Some(stop),
            served: Some(served),
        }
    }

    async fn exchange(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> Reply {
        let address = self.address;
        let method = method.to_owned();
        let path = path.to_owned();
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        tokio::task::spawn_blocking(move || spoken(address, &method, &path, &headers))
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

fn spoken(address: SocketAddr, method: &str, path: &str, headers: &[(String, String)]) -> Reply {
    let mut stream = TcpStream::connect(address).expect("the announced address accepts");
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("a read timeout is set");
    let mut head = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .expect("the request is written");
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
        .parse::<u16>()
        .expect("a status code is a number");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Reply {
        status: StatusCode::from_u16(status).expect("a known status code"),
        headers,
        body: body.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn live_the_asked_representation_is_served_over_a_socket() {
    let running = Running::started(seeded().await).await;
    let asked = running
        .exchange("GET", "/Patient/rp-1", &[("accept", "application/json")])
        .await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    assert_eq!(header(&asked, "content-type"), "application/json");

    let xml = running
        .exchange(
            "GET",
            "/Patient/rp-1",
            &[("accept", "application/fhir+xml")],
        )
        .await;
    assert_eq!(xml.status, StatusCode::OK, "{}", xml.body);
    assert_eq!(header(&xml, "content-type"), "application/fhir+xml");
    assert!(xml.body.starts_with("<Patient"), "{}", xml.body);

    let pretty = running
        .exchange("GET", "/Patient/rp-1?_pretty=true", &[])
        .await;
    assert_eq!(pretty.status, StatusCode::OK, "{}", pretty.body);
    assert!(pretty.body.contains("\n  \""), "{}", pretty.body);

    running.stopped().await;
}
