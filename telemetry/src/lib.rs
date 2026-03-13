pub mod access;
pub mod dimension;
pub mod event;
pub mod limit;
pub mod metric;

use std::sync::Arc;

pub use access::{Admission, Scrape};
pub use dimension::{Dimensions, Operation, Outcome};
pub use event::{Alarm, Event, Held, Recorded, Silent, Sink, Stream, Traced, Traces};
pub use fhir_core::CorrelationId;
pub use limit::{Limiter, Ticker, BUDGET, WINDOW_MS};
pub use metric::{Metrics, BUCKETS};

pub struct Telemetry {
    sink: Arc<dyn Sink>,
    limiter: Limiter,
    metrics: Metrics,
    alarm: Option<Arc<dyn Alarm>>,
    traces: Option<Arc<dyn Traces>>,
}

impl Telemetry {
    pub fn new(sink: Arc<dyn Sink>, ticker: Ticker) -> Telemetry {
        Telemetry {
            sink,
            limiter: Limiter::new(ticker),
            metrics: Metrics::new(),
            alarm: None,
            traces: None,
        }
    }

    pub fn tracing(self, traces: Arc<dyn Traces>) -> Telemetry {
        Telemetry {
            traces: Some(traces),
            ..self
        }
    }

    pub fn alarming(self, alarm: Arc<dyn Alarm>) -> Telemetry {
        Telemetry {
            alarm: Some(alarm),
            ..self
        }
    }

    pub fn silent() -> Telemetry {
        Telemetry::new(Arc::new(Silent), Arc::new(|| 0))
    }

    pub fn record(&self, dimensions: Dimensions, millis: u64) {
        self.emit(Event::of(dimensions, millis));
    }

    pub fn record_for(
        &self,
        dimensions: Dimensions,
        millis: u64,
        correlation: Option<CorrelationId>,
    ) {
        self.emit(Event::of(dimensions, millis).tied(correlation));
    }

    fn emit(&self, event: Event) {
        self.metrics.observe(event.dimensions, event.millis);
        let line = event.line();
        if self.limiter.admits(event.dimensions) {
            self.sink.write(&line);
        }

        if event.dimensions.outcome == Outcome::ServerFault {
            self.alert(&line);
        }

        if let Some(traces) = &self.traces {
            traces.span(&event);
        }
    }

    fn alert(&self, line: &str) {
        let Some(alarm) = &self.alarm else {
            return;
        };
        if let Err(reason) = alarm.raise(line) {
            self.sink
                .write(&format!("alert=failed reason={reason:?} for {line}"));
        }
    }

    pub fn series(&self) -> usize {
        self.metrics.measured()
    }

    pub fn count(&self, dimensions: Dimensions) -> u64 {
        self.metrics.count(dimensions)
    }

    pub fn exposition(&self) -> String {
        self.metrics.exposition(self.suppressed())
    }

    pub fn suppressed(&self) -> u64 {
        self.limiter.suppressed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_recorder_still_counts() {
        let telemetry = Telemetry::silent();
        telemetry.record(Dimensions::of(Operation::Read, Outcome::Success), 3);
        assert_eq!(telemetry.series(), 1);
    }

    #[test]
    fn measuring_every_label_set_reaches_the_bound() {
        let telemetry = Telemetry::silent();
        for slot in 0..Dimensions::COUNT {
            telemetry.record(Dimensions::at(slot), 1);
        }
        assert_eq!(telemetry.series(), Dimensions::COUNT);
    }
}
