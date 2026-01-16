use fhir_core::Error;
use fhir_store::{system_ticker, JobStore, Ticker};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub stalled: i64,
    pub defragment: i64,
}

impl Default for Schedule {
    fn default() -> Schedule {
        Schedule {
            stalled: 5_000,
            defragment: 60_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sweep {
    pub reclaimed: usize,
    pub compacted: usize,
}

impl Sweep {
    pub fn is_empty(&self) -> bool {
        self.reclaimed == 0 && self.compacted == 0
    }
}

pub struct Watchdog {
    jobs: Arc<dyn JobStore>,
    schedule: Schedule,
    ticker: Ticker,
    stalled_at: AtomicI64,
    defragment_at: AtomicI64,
}

impl Watchdog {
    pub fn new(jobs: Arc<dyn JobStore>) -> Watchdog {
        Watchdog {
            jobs,
            schedule: Schedule::default(),
            ticker: system_ticker(),
            stalled_at: AtomicI64::new(i64::MIN),
            defragment_at: AtomicI64::new(i64::MIN),
        }
    }

    pub fn with_schedule(self, schedule: Schedule) -> Watchdog {
        Watchdog { schedule, ..self }
    }

    pub fn with_ticker(self, ticker: Ticker) -> Watchdog {
        Watchdog { ticker, ..self }
    }

    fn due(&self, held: &AtomicI64, every: i64, now: i64) -> bool {
        let last = held.load(Ordering::SeqCst);
        match last == i64::MIN || now - last >= every.max(1) {
            true => {
                held.store(now, Ordering::SeqCst);
                true
            }
            false => false,
        }
    }

    pub async fn sweep(&self) -> Result<Sweep, Error> {
        let now = (self.ticker)();
        let mut swept = Sweep::default();
        if self.due(&self.stalled_at, self.schedule.stalled, now) {
            swept.reclaimed = self.jobs.reclaim().await?.len();
        }
        if self.due(&self.defragment_at, self.schedule.defragment, now) {
            swept.compacted = self.jobs.defragment().await?;
        }
        Ok(swept)
    }
}
