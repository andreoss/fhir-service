use fhir_host::otlp::Collector;
use fhir_telemetry::{Dimensions, Held, Operation, Outcome, Telemetry, Traces};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(10);

fn collecting(count: usize) -> (String, Receiver<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = listener.local_addr().expect("an address").to_string();
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for _ in 0..count {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut stream = stream;
            let _ = stream.set_read_timeout(Some(PATIENCE));
            let mut held = Vec::new();
            let _ = stream.read_to_end(&mut held);
            let text = String::from_utf8_lossy(&held).into_owned();
            let path = text
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let body = text
                .split_once("\r\n\r\n")
                .map(|(_, body)| body.to_owned())
                .unwrap_or_default();
            let _ = stream.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n");
            let parsed = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
            let _ = sender.send((path, parsed));
        }
    });
    (address, receiver)
}

fn correlation() -> fhir_core::CorrelationId {
    fhir_core::CorrelationId::parse("0123456789abcdef0123456789abcdef").expect("a correlation id")
}

#[test]
fn a_request_becomes_a_span_the_collector_reads() {
    let (address, received) = collecting(1);
    let collector = Collector::parse(&address).expect("an address");
    let telemetry = Telemetry::new(Held::default().sink(), Arc::new(|| 0))
        .tracing(Arc::new(collector) as Arc<dyn Traces>);
    telemetry.record_for(
        Dimensions::of(Operation::Read, Outcome::Success),
        7,
        Some(correlation()),
    );

    let (path, body) = received
        .recv_timeout(PATIENCE)
        .expect("the collector was called");
    assert_eq!(path, "/v1/traces");
    let span = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(span["name"], "read");
    assert_eq!(
        span["traceId"], "0123456789abcdef0123456789abcdef",
        "the correlation id this build already carries is the trace id, so a \
         span and a log line can be put side by side"
    );
    assert_eq!(span["spanId"].as_str().map(str::len), Some(16));
    let attributes = span["attributes"].as_array().expect("attributes");
    assert_eq!(
        attributes.len(),
        2,
        "the operation and the outcome, nothing else"
    );
    assert_eq!(attributes[0]["value"]["stringValue"], "read");
    assert_eq!(attributes[1]["value"]["stringValue"], "success");
    assert_eq!(span["status"]["code"], 1);

    let started = span["startTimeUnixNano"]
        .as_str()
        .and_then(|held| held.parse::<u128>().ok())
        .expect("a start");
    let ended = span["endTimeUnixNano"]
        .as_str()
        .and_then(|held| held.parse::<u128>().ok())
        .expect("an end");
    assert_eq!(
        ended - started,
        7_000_000,
        "seven milliseconds of nanoseconds"
    );
}

#[test]
fn a_failure_is_a_span_with_a_failed_status() {
    let (address, received) = collecting(1);
    let collector = Collector::parse(&address).expect("an address");
    let telemetry = Telemetry::new(Held::default().sink(), Arc::new(|| 0))
        .tracing(Arc::new(collector) as Arc<dyn Traces>);
    telemetry.record_for(
        Dimensions::of(Operation::Update, Outcome::ServerFault),
        3,
        Some(correlation()),
    );
    let (_, body) = received
        .recv_timeout(PATIENCE)
        .expect("the collector was called");
    let span = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(span["status"]["code"], 2);
    assert_eq!(span["name"], "update");
}

#[test]
fn the_counts_are_pushed_as_otlp_sums() {
    let (address, received) = collecting(1);
    let collector = Collector::parse(&address).expect("an address");
    let telemetry = Telemetry::new(Held::default().sink(), Arc::new(|| 0));
    telemetry.record(Dimensions::of(Operation::Read, Outcome::Success), 1);
    telemetry.record(Dimensions::of(Operation::Read, Outcome::Success), 2);
    telemetry.record(Dimensions::of(Operation::Create, Outcome::ClientFault), 1);

    collector
        .push_metrics(&telemetry)
        .expect("the push is made");
    let (path, body) = received
        .recv_timeout(PATIENCE)
        .expect("the collector was called");
    assert_eq!(path, "/v1/metrics");
    let metric = &body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0];
    assert_eq!(metric["name"], "fhir.requests");
    let points = metric["sum"]["dataPoints"].as_array().expect("points");
    assert_eq!(points.len(), 2, "one per label set that was measured");
    let read = points
        .iter()
        .find(|point| point["attributes"][0]["value"]["stringValue"] == "read")
        .expect("the reads are there");
    assert_eq!(read["asInt"], "2");
}

#[test]
fn an_instance_naming_no_collector_sends_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = listener.local_addr().expect("an address");
    let telemetry = Telemetry::new(Held::default().sink(), Arc::new(|| 0));
    telemetry.record(Dimensions::of(Operation::Read, Outcome::Success), 1);
    listener
        .set_nonblocking(true)
        .expect("the listener is asked not to wait");
    assert!(
        listener.accept().is_err(),
        "nothing was sent to {address}, because nothing was configured"
    );
}

#[test]
fn a_collector_that_is_not_listening_does_not_stop_the_instance() {
    let collector = Collector::parse("127.0.0.1:1").expect("an address");
    let held = Held::default();
    let telemetry =
        Telemetry::new(held.sink(), Arc::new(|| 0)).tracing(Arc::new(collector) as Arc<dyn Traces>);
    telemetry.record_for(
        Dimensions::of(Operation::Read, Outcome::Success),
        1,
        Some(correlation()),
    );
    assert_eq!(
        held.lines().len(),
        1,
        "the request was recorded as it always was; the export failed quietly"
    );
    let _ = TcpStream::connect("127.0.0.1:1");
}
