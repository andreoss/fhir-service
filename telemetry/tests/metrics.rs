use fhir_store::StepTicker;
use fhir_telemetry::{Dimensions, Held, Operation, Outcome, Telemetry, BUCKETS};

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
        assert!(text.contains(&format!("operation_failure_total{{operation=\"{name}\"}} 1")));
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
    assert!(telemetry
        .exposition()
        .contains(&format!("telemetry_suppressed_total {}", fhir_telemetry::BUDGET)));
}

#[test]
fn an_untouched_recorder_reports_no_operation() {
    let (_sink, telemetry) = telemetry();
    let text = telemetry.exposition();
    assert!(!text.contains("fhir_operation_total{"));
    assert!(text.contains("telemetry_suppressed_total 0"));
}
