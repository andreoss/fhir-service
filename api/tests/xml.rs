use axum::body::Body;
use axum::http::{Request, StatusCode};
use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use http_body_util::BodyExt;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceExt;

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const XML: &str = "application/fhir+xml";

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

fn json(reply: &Reply) -> Value {
    serde_json::from_str(&reply.body).unwrap_or(Value::Null)
}

fn patient(id: &str) -> Vec<u8> {
    format!(r#"{{"resourceType":"Patient","id":"{id}","active":true}}"#).into_bytes()
}

fn patient_xml(id: &str, active: &str) -> Vec<u8> {
    format!(
        r#"<Patient xmlns="http://hl7.org/fhir"><id value="{id}"/><active value="{active}"/></Patient>"#
    )
    .into_bytes()
}

async fn seeded() -> Service {
    let app = service();
    request(&app, "POST", "/Patient", &[], &patient("xa-1")).await;
    app
}

#[tokio::test]
async fn an_accept_header_of_xml_answers_xml() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/Patient/xa-1",
        &[("accept", "application/fhir+xml")],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), XML, "{}", reply.body);
    assert!(reply.body.starts_with("<Patient"), "{}", reply.body);
    assert!(reply.body.contains("value=\"xa-1\""), "{}", reply.body);
}

#[tokio::test]
async fn the_xml_answer_reads_back_as_the_resource() {
    let app = seeded().await;
    let asked = request(&app, "GET", "/Patient/xa-1", &[("accept", XML)], &[]).await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    let default = request(&app, "GET", "/Patient/xa-1", &[], &[]).await;
    let read_back =
        fhir_core::xml::from_xml(FhirVersion::R4, &asked.body).expect("the answer is read as xml");
    assert_eq!(read_back, json(&default));
}

#[tokio::test]
async fn the_format_parameter_asks_for_xml() {
    let app = seeded().await;
    for format in ["xml", "application/fhir+xml", "application/xml+fhir"] {
        let reply = request(
            &app,
            "GET",
            &format!("/Patient/xa-1?_format={format}"),
            &[],
            &[],
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{format}: {}", reply.body);
        assert_eq!(
            header(&reply, "content-type"),
            XML,
            "{format}: {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn a_bundle_is_answered_in_xml() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient?_format=xml", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), XML, "{}", reply.body);
    assert!(reply.body.starts_with("<Bundle"), "{}", reply.body);
    let read_back =
        fhir_core::xml::from_xml(FhirVersion::R4, &reply.body).expect("the bundle is read as xml");
    assert_eq!(read_back["resourceType"], "Bundle");
    assert_eq!(read_back["entry"][0]["resource"]["resourceType"], "Patient");
}

#[tokio::test]
async fn a_client_creates_a_resource_from_xml() {
    let app = service();
    let created = request(
        &app,
        "POST",
        "/Patient",
        &[("content-type", XML)],
        &patient_xml("xc-1", "false"),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let read = request(&app, "GET", "/Patient/xc-1", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(json(&read)["active"], false);
}

#[tokio::test]
async fn a_client_updates_a_resource_from_xml() {
    let app = seeded().await;
    let updated = request(
        &app,
        "PUT",
        "/Patient/xa-1",
        &[("content-type", XML)],
        &patient_xml("xa-1", "false"),
    )
    .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    let read = request(&app, "GET", "/Patient/xa-1", &[], &[]).await;
    assert_eq!(json(&read)["active"], false);
}

#[tokio::test]
async fn an_operation_outcome_is_answered_in_xml() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient/absent", &[("accept", XML)], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), XML, "{}", reply.body);
    let read_back =
        fhir_core::xml::from_xml(FhirVersion::R4, &reply.body).expect("the outcome is read as xml");
    assert_eq!(read_back["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn a_malformed_xml_body_is_refused() {
    let app = service();
    let reply = request(
        &app,
        "POST",
        "/Patient",
        &[("content-type", XML), ("accept", XML)],
        b"<Patient><id value=\"xm-1\"/>",
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), XML, "{}", reply.body);
    assert!(reply.body.contains("malformed xml"), "{}", reply.body);
    let read = request(&app, "GET", "/Patient/xm-1", &[], &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND, "{}", read.body);
}

#[tokio::test]
async fn an_xml_body_naming_another_type_is_refused() {
    let app = service();
    let body =
        br#"<Observation xmlns="http://hl7.org/fhir"><id value="xo-1"/><status value="final"/></Observation>"#;
    let reply = request(
        &app,
        "POST",
        "/Patient",
        &[("content-type", XML), ("accept", XML)],
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("does not match"), "{}", reply.body);
    let read = request(&app, "GET", "/Observation/xo-1", &[], &[]).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND, "{}", read.body);
}

#[tokio::test]
async fn an_unsupported_format_is_still_refused() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient/xa-1?_format=html", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::NOT_ACCEPTABLE, "{}", reply.body);
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
    let mut head = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {}\r\n\r\n", body.len()));
    let mut sent = head.into_bytes();
    sent.extend_from_slice(body);
    stream.write_all(&sent).expect("the request is written");
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
async fn live_xml_is_served_and_accepted_over_a_socket() {
    let running = Running::started(service()).await;
    let created = running
        .exchange(
            "POST",
            "/Patient",
            &[("content-type", XML), ("accept", XML)],
            &patient_xml("xl-1", "true"),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let read = running
        .exchange("GET", "/Patient/xl-1", &[("accept", XML)], &[])
        .await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(header(&read, "content-type"), XML, "{}", read.body);
    assert!(read.body.starts_with("<Patient"), "{}", read.body);

    let again = running
        .exchange("GET", "/Patient/xl-1?_format=xml", &[], &[])
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert!(again.body.contains("value=\"xl-1\""), "{}", again.body);

    running.stopped().await;
}

#[tokio::test]
async fn a_pretty_xml_answer_is_indented_and_reads_the_same() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/Patient/xa-1?_format=xml&_pretty=true",
        &[],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), XML, "{}", reply.body);
    assert!(reply.body.starts_with("<Patient"), "{}", reply.body);
    assert!(
        reply.body.contains("\n  <id value=\"xa-1\"/>"),
        "{}",
        reply.body
    );
    assert!(
        reply.body.contains("\n  <active value=\"true\"/>"),
        "{}",
        reply.body
    );
    let pretty = fhir_core::xml::from_xml(FhirVersion::R4, &reply.body).expect("pretty is read");
    let findings = fhir_core::model::Model::of(FhirVersion::R4).check(&pretty);
    assert!(findings.is_empty(), "{findings:?}");
    let compact = request(&app, "GET", "/Patient/xa-1?_format=xml", &[], &[]).await;
    let plain = fhir_core::xml::from_xml(FhirVersion::R4, &compact.body).expect("compact is read");
    assert_eq!(pretty, plain, "{pretty} is not {plain}");
    assert!(!compact.body.contains('\n'), "{}", compact.body);
}

#[tokio::test]
async fn a_pretty_xml_answer_comes_from_the_accept_header_too() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/Patient/xa-1?_pretty=true",
        &[("accept", XML)],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(
        reply.body.contains("\n  <id value=\"xa-1\"/>"),
        "{}",
        reply.body
    );
    let asked = request(
        &app,
        "GET",
        "/Patient/xa-1?_pretty=false",
        &[("accept", XML)],
        &[],
    )
    .await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    assert!(!asked.body.contains('\n'), "{}", asked.body);
}

#[tokio::test]
async fn a_pretty_xml_outcome_is_indented() {
    let app = seeded().await;
    let reply = request(
        &app,
        "GET",
        "/Patient/absent?_format=xml&_pretty=true",
        &[],
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    assert_eq!(header(&reply, "content-type"), XML, "{}", reply.body);
    assert!(
        reply.body.starts_with("<OperationOutcome"),
        "{}",
        reply.body
    );
    assert!(reply.body.contains("\n  <issue>"), "{}", reply.body);
    let read_back =
        fhir_core::xml::from_xml(FhirVersion::R4, &reply.body).expect("the outcome is read");
    assert_eq!(read_back["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn a_pretty_xml_bundle_is_indented_and_reads_the_same() {
    let app = seeded().await;
    let reply = request(&app, "GET", "/Patient?_format=xml&_pretty=true", &[], &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains("\n  <entry>"), "{}", reply.body);
    assert!(
        reply.body.contains("\n        <id value=\"xa-1\"/>"),
        "{}",
        reply.body
    );
    let read_back =
        fhir_core::xml::from_xml(FhirVersion::R4, &reply.body).expect("the bundle is read");
    assert_eq!(read_back["entry"][0]["resource"]["id"], "xa-1");
}

#[tokio::test(flavor = "multi_thread")]
async fn live_a_pretty_xml_answer_crosses_a_socket() {
    let running = Running::started(service()).await;
    let pretty = running
        .exchange(
            "GET",
            "/Patient/xp-1?_format=xml&_pretty=true",
            &[("accept", XML)],
            &[],
        )
        .await;
    assert_eq!(pretty.status, StatusCode::NOT_FOUND, "{}", pretty.body);
    assert!(pretty.body.contains("\n  <issue>"), "{}", pretty.body);

    let created = running
        .exchange(
            "POST",
            "/Patient",
            &[("content-type", XML)],
            &patient_xml("xp-1", "true"),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let read = running
        .exchange("GET", "/Patient/xp-1?_format=xml&_pretty=true", &[], &[])
        .await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert!(
        read.body.contains("\n  <id value=\"xp-1\"/>"),
        "{}",
        read.body
    );
    let again = running
        .exchange("GET", "/Patient/xp-1?_format=xml", &[], &[])
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert!(!again.body.contains('\n'), "{}", again.body);
    assert!(
        again.body.contains("<id value=\"xp-1\"/>"),
        "{}",
        again.body
    );

    running.stopped().await;
}

#[tokio::test]
async fn an_indented_xml_body_is_accepted() {
    let app = service();
    let indented = concat!(
        "<Patient xmlns=\"http://hl7.org/fhir\">\n",
        "  <id value=\"xi-1\"/>\n",
        "  <active value=\"true\"/>\n",
        "  <name>\n",
        "    <family value=\"Smith\"/>\n",
        "  </name>\n",
        "</Patient>",
    );
    let created = request(
        &app,
        "POST",
        "/Patient",
        &[("content-type", XML)],
        indented.as_bytes(),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let read = request(&app, "GET", "/Patient/xi-1", &[], &[]).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(json(&read)["name"][0]["family"], "Smith");
}
