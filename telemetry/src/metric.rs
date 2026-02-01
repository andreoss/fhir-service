use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::dimension::{Dimensions, Operation, Outcome};

pub const BUCKETS: [u64; 9] = [5, 10, 25, 50, 100, 250, 500, 1_000, 5_000];

const PREFIX: &str = "fhir";

struct Series {
    count: AtomicU64,
    total_ms: AtomicU64,
    buckets: Vec<AtomicU64>,
}

impl Series {
    fn new() -> Series {
        Series {
            count: AtomicU64::new(0),
            total_ms: AtomicU64::new(0),
            buckets: (0..BUCKETS.len() + 1).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    fn observe(&self, millis: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ms.fetch_add(millis, Ordering::Relaxed);
        let slot = BUCKETS
            .iter()
            .position(|bound| millis <= *bound)
            .unwrap_or(BUCKETS.len());
        self.buckets[slot].fetch_add(1, Ordering::Relaxed);
    }

    fn observed(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

pub struct Metrics {
    series: Vec<Series>,
}

impl Default for Metrics {
    fn default() -> Metrics {
        Metrics::new()
    }
}

impl Metrics {
    pub fn new() -> Metrics {
        Metrics {
            series: (0..Dimensions::COUNT).map(|_| Series::new()).collect(),
        }
    }

    pub fn observe(&self, dimensions: Dimensions, millis: u64) {
        self.series[dimensions.slot()].observe(millis);
    }

    pub fn measured(&self) -> usize {
        self.series
            .iter()
            .filter(|series| series.observed() > 0)
            .count()
    }

    pub fn count(&self, dimensions: Dimensions) -> u64 {
        self.series[dimensions.slot()].observed()
    }

    pub fn failures(&self, operation: Operation) -> u64 {
        Outcome::ALL
            .into_iter()
            .filter(|outcome| outcome.is_failure())
            .map(|outcome| self.count(Dimensions::of(operation, outcome)))
            .sum()
    }

    pub fn exposition(&self, suppressed: u64) -> String {
        let mut text = String::new();
        self.counters(&mut text);
        self.failure_counters(&mut text);
        self.durations(&mut text);
        let _ = writeln!(
            text,
            "# TYPE {PREFIX}_telemetry_suppressed_total counter\n{PREFIX}_telemetry_suppressed_total {suppressed}"
        );
        text
    }

    fn counters(&self, text: &mut String) {
        let _ = writeln!(text, "# TYPE {PREFIX}_operation_total counter");
        for slot in self.reported() {
            let dimensions = Dimensions::at(slot);
            let _ = writeln!(
                text,
                "{PREFIX}_operation_total{{operation=\"{}\",outcome=\"{}\"}} {}",
                dimensions.operation.as_str(),
                dimensions.outcome.as_str(),
                self.series[slot].observed()
            );
        }
    }

    fn failure_counters(&self, text: &mut String) {
        let _ = writeln!(text, "# TYPE {PREFIX}_operation_failure_total counter");
        for operation in Operation::ALL {
            let failures = self.failures(operation);
            let measured = Outcome::ALL
                .into_iter()
                .any(|outcome| self.count(Dimensions::of(operation, outcome)) > 0);
            if measured {
                let _ = writeln!(
                    text,
                    "{PREFIX}_operation_failure_total{{operation=\"{}\"}} {failures}",
                    operation.as_str()
                );
            }
        }
    }

    fn durations(&self, text: &mut String) {
        let _ = writeln!(text, "# TYPE {PREFIX}_operation_duration_ms histogram");
        for slot in self.reported() {
            let dimensions = Dimensions::at(slot);
            let labels = format!(
                "operation=\"{}\",outcome=\"{}\"",
                dimensions.operation.as_str(),
                dimensions.outcome.as_str()
            );
            let series = &self.series[slot];
            let mut running = 0;
            for (position, bound) in BUCKETS.iter().enumerate() {
                running += series.buckets[position].load(Ordering::Relaxed);
                let _ = writeln!(
                    text,
                    "{PREFIX}_operation_duration_ms_bucket{{{labels},le=\"{bound}\"}} {running}"
                );
            }
            running += series.buckets[BUCKETS.len()].load(Ordering::Relaxed);
            let _ = writeln!(
                text,
                "{PREFIX}_operation_duration_ms_bucket{{{labels},le=\"+Inf\"}} {running}"
            );
            let _ = writeln!(
                text,
                "{PREFIX}_operation_duration_ms_sum{{{labels}}} {}",
                series.total_ms.load(Ordering::Relaxed)
            );
            let _ = writeln!(
                text,
                "{PREFIX}_operation_duration_ms_count{{{labels}}} {}",
                series.observed()
            );
        }
    }

    fn reported(&self) -> Vec<usize> {
        (0..Dimensions::COUNT)
            .filter(|slot| self.series[*slot].observed() > 0)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_counts_against_its_operation() {
        let metrics = Metrics::new();
        metrics.observe(Dimensions::of(Operation::Search, Outcome::ClientFault), 4);
        metrics.observe(Dimensions::of(Operation::Search, Outcome::ServerFault), 4);
        metrics.observe(Dimensions::of(Operation::Search, Outcome::Success), 4);
        assert_eq!(metrics.failures(Operation::Search), 2);
        assert_eq!(metrics.measured(), 3);
    }

    #[test]
    fn a_long_operation_lands_beyond_the_last_bound() {
        let metrics = Metrics::new();
        metrics.observe(Dimensions::of(Operation::Export, Outcome::Success), 9_000);
        let text = metrics.exposition(0);
        assert!(text.contains("le=\"5000\"} 0"));
        assert!(text.contains("le=\"+Inf\"} 1"));
    }
}
