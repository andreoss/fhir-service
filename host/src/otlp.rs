use fhir_core::Error;
use fhir_telemetry::{Event, Telemetry, Traces};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PATIENCE: Duration = Duration::from_secs(5);
const TRACES: &str = "/v1/traces";
const METRICS: &str = "/v1/metrics";

pub const PUSH_SECONDS: u64 = 15;

const SERVICE: &str = "fhir-service";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collector {
    address: String,
    prefix: String,
}

impl Collector {
    pub fn parse(raw: &str) -> Result<Collector, Error> {
        let held = raw.trim().trim_start_matches("http://");
        if held.starts_with("https://") {
            return Err(Error::Config(
                "the collector is reached over plain HTTP; put a proxy in front of a TLS \
                 endpoint rather than naming one here"
                    .to_owned(),
            ));
        }
        let (address, prefix) = match held.split_once('/') {
            None => (held, String::new()),
            Some((address, prefix)) => (address, format!("/{}", prefix.trim_end_matches('/'))),
        };
        if !address.contains(':') {
            return Err(Error::Config(format!(
                "the collector address {raw:?} names no port"
            )));
        }
        Ok(Collector {
            address: address.to_owned(),
            prefix,
        })
    }

    fn post(&self, path: &str, body: &Value) -> Result<(), String> {
        let body = body.to_string();
        let stream = TcpStream::connect(&self.address).map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(PATIENCE))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(PATIENCE))
            .map_err(|error| error.to_string())?;
        let mut stream = stream;
        let request = format!(
            "POST {}{path} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.prefix,
            self.address,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut answered = String::new();
        let _ = stream.read_to_string(&mut answered);
        Ok(())
    }

    pub fn push_metrics(&self, telemetry: &Telemetry) -> Result<(), String> {
        let at = now_nanos();
        let mut points = Vec::new();
        for slot in 0..fhir_telemetry::Dimensions::COUNT {
            let dimensions = fhir_telemetry::Dimensions::at(slot);
            let count = telemetry.count(dimensions);
            if count == 0 {
                continue;
            }
            points.push(json!({
                "asInt": count.to_string(),
                "timeUnixNano": at.to_string(),
                "attributes": attributes(dimensions),
            }));
        }
        if points.is_empty() {
            return Ok(());
        }
        self.post(
            METRICS,
            &json!({
                "resourceMetrics": [{
                    "resource": {"attributes": [text("service.name", SERVICE)]},
                    "scopeMetrics": [{
                        "scope": {"name": SERVICE},
                        "metrics": [{
                            "name": "fhir.requests",
                            "unit": "1",
                            "sum": {
                                "dataPoints": points,
                                "aggregationTemporality": 2,
                                "isMonotonic": true,
                            },
                        }],
                    }],
                }],
            }),
        )
    }
}

impl Traces for Collector {
    fn span(&self, event: &Event) {
        let ended = now_nanos();
        let started = ended.saturating_sub(event.millis.saturating_mul(1_000_000) as u128);
        let mut span = json!({
            "name": event.dimensions.operation.as_str(),
            "kind": 2,
            "spanId": span_id(event),
            "startTimeUnixNano": started.to_string(),
            "endTimeUnixNano": ended.to_string(),
            "attributes": attributes(event.dimensions),
            "status": {"code": status_of(event)},
        });
        if let Some(correlation) = &event.correlation {
            span["traceId"] = json!(correlation.to_string());
        }
        let _ = self.post(
            TRACES,
            &json!({
                "resourceSpans": [{
                    "resource": {"attributes": [text("service.name", SERVICE)]},
                    "scopeSpans": [{
                        "scope": {"name": SERVICE},
                        "spans": [span],
                    }],
                }],
            }),
        );
    }
}

fn span_id(event: &Event) -> String {
    match &event.correlation {
        Some(correlation) => correlation.to_string().chars().take(16).collect(),
        None => "0".repeat(16),
    }
}

fn status_of(event: &Event) -> u8 {
    match event.dimensions.outcome.is_failure() {
        true => 2,
        false => 1,
    }
}

fn attributes(dimensions: fhir_telemetry::Dimensions) -> Vec<Value> {
    vec![
        text("fhir.operation", dimensions.operation.as_str()),
        text("fhir.outcome", dimensions.outcome.as_str()),
    ]
}

fn text(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|held| held.as_nanos())
        .unwrap_or_default()
}

pub fn spawn_metrics(collector: Collector, telemetry: Arc<Telemetry>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(PUSH_SECONDS));
        loop {
            ticker.tick().await;
            let collector = collector.clone();
            let telemetry = Arc::clone(&telemetry);

            let _ = tokio::task::spawn_blocking(move || collector.push_metrics(&telemetry)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_read_with_or_without_a_prefix() {
        let bare = Collector::parse("127.0.0.1:4318").expect("an address");
        assert_eq!(bare.prefix, "");
        let held = Collector::parse("http://127.0.0.1:4318/otel/").expect("an address");
        assert_eq!(held.address, "127.0.0.1:4318");
        assert_eq!(held.prefix, "/otel");
    }

    #[test]
    fn what_is_no_address_fails_fast() {
        assert!(Collector::parse("nowhere").is_err());
        assert!(Collector::parse("https://collector.example.org").is_err());
    }

    #[test]
    fn a_span_carries_the_operation_the_outcome_and_nothing_else() {
        let dimensions = fhir_telemetry::Dimensions::of(
            fhir_telemetry::Operation::Read,
            fhir_telemetry::Outcome::Success,
        );
        let held = attributes(dimensions);
        assert_eq!(held.len(), 2);
        assert_eq!(held[0]["key"], "fhir.operation");
        assert_eq!(held[1]["value"]["stringValue"], "success");
    }
}
