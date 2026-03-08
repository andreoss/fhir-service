use fhir_store::StepTicker;
use fhir_telemetry::{Dimensions, Held, Operation, Outcome, Telemetry, BUDGET};

#[test]
fn every_dimension_pair_is_one_of_a_fixed_set() {
    let mut seen = Vec::new();
    for operation in Operation::ALL {
        for outcome in Outcome::ALL {
            let slot = Dimensions { operation, outcome }.slot();
            assert!(slot < Dimensions::COUNT);
            assert!(!seen.contains(&slot));
            seen.push(slot);
        }
    }
    assert_eq!(seen.len(), Dimensions::COUNT);
}

#[test]
fn a_flood_of_distinct_requests_adds_no_dimension() {
    let sink = Held::default();
    let clock = StepTicker::starting_at(0);
    let telemetry = Telemetry::new(sink.sink(), clock.ticker());
    for step in 0..5000 {
        clock.advance(1000);
        telemetry.record(
            Dimensions {
                operation: Operation::Search,
                outcome: Outcome::Success,
            },
            step % 97,
        );
    }
    assert_eq!(telemetry.series(), 1);
    assert!(telemetry.series() <= Dimensions::COUNT);
}

#[test]
fn events_beyond_the_budget_are_dropped_and_counted() {
    let sink = Held::default();
    let clock = StepTicker::starting_at(0);
    let telemetry = Telemetry::new(sink.sink(), clock.ticker());
    let dimensions = Dimensions {
        operation: Operation::Bundle,
        outcome: Outcome::Success,
    };
    for _ in 0..(BUDGET * 3) {
        telemetry.record(dimensions, 1);
    }
    assert_eq!(sink.lines().len(), BUDGET as usize);
    assert_eq!(telemetry.suppressed(), (BUDGET * 2) as u64);
}

#[test]
fn a_new_window_admits_events_again() {
    let sink = Held::default();
    let clock = StepTicker::starting_at(0);
    let telemetry = Telemetry::new(sink.sink(), clock.ticker());
    let dimensions = Dimensions {
        operation: Operation::Export,
        outcome: Outcome::ServerFault,
    };
    for _ in 0..(BUDGET * 2) {
        telemetry.record(dimensions, 1);
    }
    clock.advance(fhir_telemetry::WINDOW_MS);
    telemetry.record(dimensions, 1);
    assert_eq!(sink.lines().len(), (BUDGET + 1) as usize);
}

#[test]
fn one_busy_dimension_does_not_silence_another() {
    let sink = Held::default();
    let clock = StepTicker::starting_at(0);
    let telemetry = Telemetry::new(sink.sink(), clock.ticker());
    let busy = Dimensions {
        operation: Operation::Search,
        outcome: Outcome::Success,
    };
    for _ in 0..(BUDGET * 2) {
        telemetry.record(busy, 1);
    }
    telemetry.record(
        Dimensions {
            operation: Operation::Import,
            outcome: Outcome::ServerFault,
        },
        7,
    );
    let lines = sink.lines();
    assert!(lines.iter().any(|line| line.contains("operation=import")));
}

#[test]
fn an_event_line_carries_only_names_and_numbers() {
    let sink = Held::default();
    let clock = StepTicker::starting_at(0);
    let telemetry = Telemetry::new(sink.sink(), clock.ticker());
    telemetry.record(
        Dimensions {
            operation: Operation::Read,
            outcome: Outcome::ClientFault,
        },
        42,
    );
    let lines = sink.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0],
        "operation=read outcome=client_fault duration_ms=42"
    );
}

#[test]
fn a_status_reports_the_side_that_failed() {
    assert_eq!(Outcome::of_status(200), Outcome::Success);
    assert_eq!(Outcome::of_status(304), Outcome::Success);
    assert_eq!(Outcome::of_status(404), Outcome::ClientFault);
    assert_eq!(Outcome::of_status(500), Outcome::ServerFault);
}
