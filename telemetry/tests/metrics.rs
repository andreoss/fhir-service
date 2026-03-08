use fhir_store::StepTicker;
use fhir_telemetry::{Dimensions, Held, Operation, Outcome, Recorded, Telemetry, BUCKETS};
use std::sync::Arc;

fn telemetry() -> (Held, Telemetry) {
    let sink = Held::default();
    let clock = StepTicker::starting_at(0);
    (sink.clone(), Telemetry::new(sink.sink(), clock.ticker()))
}

#[test]
fn every_measured_operation_reports_count_latency_and_failure() {
    let (_sink, telemetry) = telemetry();
    for operation in [
        Operation::Bundle,
        Operation::Export,
        Operation::Import,
        Operation::Reindex,
        Operation::Search,
    ] {
        telemetry.record(Dimensions::of(operation, Outcome::Success), 12);
        telemetry.record(Dimensions::of(operation, Outcome::ServerFault), 30);
    }
    let text = telemetry.exposition();
    for name in ["bundle", "export", "import", "reindex", "search"] {
        assert!(text.contains(&format!(
            "operation_total{{operation=\"{name}\",outcome=\"success\"}} 1"
        )));
        assert!(text.contains(&format!(
            "operation_failure_total{{operation=\"{name}\"}} 1"
        )));
        assert!(text.contains(&format!(
            "operation_duration_ms_count{{operation=\"{name}\",outcome=\"success\"}} 1"
        )));
        assert!(text.contains(&format!(
            "operation_duration_ms_sum{{operation=\"{name}\",outcome=\"success\"}} 12"
        )));
    }
}

#[test]
fn a_duration_lands_in_the_bucket_it_belongs_to() {
    let (_sink, telemetry) = telemetry();
    let dimensions = Dimensions::of(Operation::Search, Outcome::Success);
    telemetry.record(dimensions, 7);
    let text = telemetry.exposition();
    assert!(text.contains("le=\"5\"} 0"));
    assert!(text.contains("le=\"10\"} 1"));
    assert!(text.contains("le=\"+Inf\"} 1"));
}

#[test]
fn the_reported_series_stay_within_the_bound() {
    let (_sink, telemetry) = telemetry();
    for step in 0..4000u64 {
        telemetry.record(Dimensions::at((step as usize) % Dimensions::COUNT), step);
    }
    let text = telemetry.exposition();
    let counters = text
        .lines()
        .filter(|line| line.starts_with("fhir_operation_total"))
        .count();
    let buckets = text
        .lines()
        .filter(|line| line.contains("_duration_ms_bucket"))
        .count();
    assert_eq!(counters, Dimensions::COUNT);
    assert_eq!(buckets, Dimensions::COUNT * (BUCKETS.len() + 1));
}

#[test]
fn what_the_rate_limit_dropped_is_reported() {
    let (_sink, telemetry) = telemetry();
    let dimensions = Dimensions::of(Operation::Read, Outcome::Success);
    for _ in 0..(fhir_telemetry::BUDGET * 2) {
        telemetry.record(dimensions, 1);
    }
    assert!(telemetry.exposition().contains(&format!(
        "telemetry_suppressed_total {}",
        fhir_telemetry::BUDGET
    )));
}

#[test]
fn an_untouched_recorder_reports_no_operation() {
    let (_sink, telemetry) = telemetry();
    let text = telemetry.exposition();
    assert!(!text.contains("fhir_operation_total{"));
    assert!(text.contains("telemetry_suppressed_total 0"));
}

#[test]
fn a_failure_of_the_instance_raises_an_alert_once() {
    let held = Held::default();
    let alarm = Recorded::default();
    let telemetry = Telemetry::new(held.sink(), Arc::new(|| 0)).alarming(alarm.alarm());
    telemetry.record(Dimensions::of(Operation::Read, Outcome::ServerFault), 5);
    assert_eq!(alarm.raised().len(), 1, "one failure, one alert");
    assert!(
        alarm.raised()[0].contains("outcome=server_fault"),
        "and it carries the line it was raised for: {:?}",
        alarm.raised()
    );
    telemetry.record(Dimensions::of(Operation::Read, Outcome::ServerFault), 6);
    assert_eq!(alarm.raised().len(), 2, "each failure, an alert");
}

#[test]
fn what_a_client_got_wrong_raises_nothing() {
    let alarm = Recorded::default();
    let telemetry = Telemetry::new(Held::default().sink(), Arc::new(|| 0)).alarming(alarm.alarm());
    telemetry.record(Dimensions::of(Operation::Read, Outcome::ClientFault), 1);
    telemetry.record(Dimensions::of(Operation::Read, Outcome::Success), 1);
    assert!(
        alarm.raised().is_empty(),
        "a request a client got wrong is not an outage: {:?}",
        alarm.raised()
    );
}

#[test]
fn an_alert_is_raised_even_when_the_log_is_rate_limited() {
    let held = Held::default();
    let alarm = Recorded::default();
    let telemetry = Telemetry::new(held.sink(), Arc::new(|| 0)).alarming(alarm.alarm());
    let failures = fhir_telemetry::BUDGET + 5;
    for _ in 0..failures {
        telemetry.record(Dimensions::of(Operation::Read, Outcome::ServerFault), 1);
    }
    assert!(
        telemetry.suppressed() > 0,
        "the log is holding lines back, which is the point of this test"
    );
    assert_eq!(
        alarm.raised().len(),
        failures as usize,
        "and every failure was still alerted"
    );
}

#[test]
fn a_failure_to_alert_is_itself_logged() {
    struct Refusing;
    impl fhir_telemetry::Alarm for Refusing {
        fn raise(&self, _line: &str) -> Result<(), String> {
            Err("the address refused a connection".to_owned())
        }
    }
    let held = Held::default();
    let telemetry = Telemetry::new(held.sink(), Arc::new(|| 0)).alarming(Arc::new(Refusing));
    telemetry.record(Dimensions::of(Operation::Read, Outcome::ServerFault), 1);
    let lines = held.lines();
    assert!(
        lines.iter().any(|line| line.starts_with("alert=failed")),
        "the instance says it could not call for help: {lines:?}"
    );
}
