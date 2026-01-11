use fhir_core::FhirInstant;
use std::sync::Arc;

pub type Clock = Arc<dyn Fn() -> FhirInstant + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .ok()
            .and_then(|text| FhirInstant::parse(&text).ok())
            .unwrap_or_else(epoch)
    })
}

fn epoch() -> FhirInstant {
    FhirInstant::parse("1970-01-01T00:00:00+00:00").expect("epoch instant is valid")
}


pub type Ticker = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_ticker() -> Ticker {
    Arc::new(|| {
        let now = time::OffsetDateTime::now_utc() - time::OffsetDateTime::UNIX_EPOCH;
        now.whole_milliseconds() as i64
    })
}

#[derive(Debug, Clone, Default)]
pub struct StepTicker(Arc<std::sync::atomic::AtomicI64>);

impl StepTicker {
    pub fn starting_at(millis: i64) -> StepTicker {
        StepTicker(Arc::new(std::sync::atomic::AtomicI64::new(millis)))
    }

    pub fn advance(&self, millis: i64) -> i64 {
        self.0.fetch_add(millis, std::sync::atomic::Ordering::SeqCst) + millis
    }

    pub fn now(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn ticker(&self) -> Ticker {
        let held = Arc::clone(&self.0);
        Arc::new(move || held.load(std::sync::atomic::Ordering::SeqCst))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_reads_a_valid_instant() {
        let clock = system_clock();
        assert!(clock().key() > epoch().key());
    }
}

#[cfg(test)]
mod ticker_tests {
    use super::*;

    #[test]
    fn a_stepped_ticker_moves_only_when_it_is_advanced() {
        let ticker = StepTicker::starting_at(500);
        let read = ticker.ticker();
        assert_eq!(read(), 500);
        assert_eq!(ticker.advance(250), 750);
        assert_eq!(read(), 750);
        assert_eq!(ticker.now(), 750);
    }

    #[test]
    fn the_system_ticker_runs_past_the_epoch() {
        assert!(system_ticker()() > 0);
    }
}
