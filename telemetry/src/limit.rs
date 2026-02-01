use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::dimension::Dimensions;

pub type Ticker = std::sync::Arc<dyn Fn() -> i64 + Send + Sync>;

pub const WINDOW_MS: i64 = 1_000;

pub const BUDGET: u32 = 20;

#[derive(Debug, Clone, Copy)]
struct Cell {
    opened: i64,
    spent: u32,
}

pub struct Limiter {
    ticker: Ticker,
    window: i64,
    budget: u32,
    cells: Vec<Mutex<Cell>>,
    suppressed: AtomicU64,
}

impl Limiter {
    pub fn new(ticker: Ticker) -> Limiter {
        Limiter::with_budget(ticker, BUDGET, WINDOW_MS)
    }

    pub fn with_budget(ticker: Ticker, budget: u32, window: i64) -> Limiter {
        let cells = (0..Dimensions::COUNT)
            .map(|_| {
                Mutex::new(Cell {
                    opened: i64::MIN,
                    spent: 0,
                })
            })
            .collect();
        Limiter {
            ticker,
            window,
            budget,
            cells,
            suppressed: AtomicU64::new(0),
        }
    }

    pub fn admits(&self, dimensions: Dimensions) -> bool {
        let now = (self.ticker)();
        let cell = &self.cells[dimensions.slot()];
        let mut held = match cell.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        if held.opened == i64::MIN || now.saturating_sub(held.opened) >= self.window {
            held.opened = now;
            held.spent = 0;
        }
        match held.spent < self.budget {
            true => {
                held.spent += 1;
                true
            }
            false => {
                self.suppressed.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn suppressed(&self) -> u64 {
        self.suppressed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dimension::{Operation, Outcome};
    use std::sync::atomic::AtomicI64;
    use std::sync::Arc;

    fn stepped(held: &Arc<AtomicI64>) -> Ticker {
        let held = Arc::clone(held);
        Arc::new(move || held.load(Ordering::SeqCst))
    }

    #[test]
    fn a_budget_is_spent_and_then_refused() {
        let now = Arc::new(AtomicI64::new(0));
        let limiter = Limiter::with_budget(stepped(&now), 2, 100);
        let dimensions = Dimensions::of(Operation::Read, Outcome::Success);
        assert!(limiter.admits(dimensions));
        assert!(limiter.admits(dimensions));
        assert!(!limiter.admits(dimensions));
        assert_eq!(limiter.suppressed(), 1);
    }

    #[test]
    fn a_later_window_starts_a_fresh_budget() {
        let now = Arc::new(AtomicI64::new(0));
        let limiter = Limiter::with_budget(stepped(&now), 1, 100);
        let dimensions = Dimensions::of(Operation::Search, Outcome::ClientFault);
        assert!(limiter.admits(dimensions));
        assert!(!limiter.admits(dimensions));
        now.store(100, Ordering::SeqCst);
        assert!(limiter.admits(dimensions));
    }

    #[test]
    fn each_label_set_holds_its_own_budget() {
        let now = Arc::new(AtomicI64::new(0));
        let limiter = Limiter::with_budget(stepped(&now), 1, 100);
        assert!(limiter.admits(Dimensions::of(Operation::Read, Outcome::Success)));
        assert!(limiter.admits(Dimensions::of(Operation::Read, Outcome::ServerFault)));
        assert!(!limiter.admits(Dimensions::of(Operation::Read, Outcome::Success)));
    }
}
