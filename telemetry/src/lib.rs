
pub mod dimension;
pub mod event;
pub mod limit;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub use dimension::{Dimensions, Operation, Outcome};
pub use event::{Event, Held, Silent, Sink, Stream};
pub use limit::{Limiter, Ticker, BUDGET, WINDOW_MS};

pub struct Telemetry {
    sink: Arc<dyn Sink>,
    limiter: Limiter,
    counts: Vec<AtomicU64>,
}

impl Telemetry {
    pub fn new(sink: Arc<dyn Sink>, ticker: Ticker) -> Telemetry {
        Telemetry {
            sink,
            limiter: Limiter::new(ticker),
            counts: (0..Dimensions::COUNT).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    pub fn silent() -> Telemetry {
        Telemetry::new(Arc::new(Silent), Arc::new(|| 0))
    }

    pub fn record(&self, dimensions: Dimensions, millis: u64) {
        self.counts[dimensions.slot()].fetch_add(1, Ordering::Relaxed);
        if self.limiter.admits(dimensions) {
            self.sink.write(&Event { dimensions, millis }.line());
        }
    }

    pub fn series(&self) -> usize {
        self.counts
            .iter()
            .filter(|count| count.load(Ordering::Relaxed) > 0)
            .count()
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
