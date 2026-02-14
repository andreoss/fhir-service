use fhir_core::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const DEFAULT_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

pub struct Admission {
    _permit: OwnedSemaphorePermit,
    running: Arc<AtomicUsize>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.running.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Throttle {
    permits: Arc<Semaphore>,
    reserved: Arc<Semaphore>,
    limit: usize,
    budget: std::time::Duration,
    running: Arc<AtomicUsize>,
    peak: AtomicUsize,
}

impl Throttle {
    pub fn new(limit: usize) -> Throttle {
        let limit = limit.max(1);
        Throttle {
            permits: Arc::new(Semaphore::new(limit)),
            reserved: Arc::new(Semaphore::new(0)),
            limit,
            budget: DEFAULT_BUDGET,
            running: Arc::new(AtomicUsize::new(0)),
            peak: AtomicUsize::new(0),
        }
    }

    pub fn waiting(self, budget: std::time::Duration) -> Throttle {
        Throttle { budget, ..self }
    }

    pub fn reserving(self, places: usize) -> Throttle {
        let places = places.min(self.limit.saturating_sub(1));
        Throttle {
            permits: Arc::new(Semaphore::new(self.limit - places)),
            reserved: Arc::new(Semaphore::new(places)),
            ..self
        }
    }

    pub fn budget(&self) -> std::time::Duration {
        self.budget
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    pub async fn admit(&self) -> Result<Admission, Error> {
        self.taken(Arc::clone(&self.permits)).await
    }

    pub async fn admit_cheap(&self) -> Result<Admission, Error> {
        match Arc::clone(&self.reserved).try_acquire_owned() {
            Ok(permit) => Ok(self.held(permit)),
            Err(_) => self.taken(Arc::clone(&self.permits)).await,
        }
    }

    async fn taken(&self, lane: Arc<Semaphore>) -> Result<Admission, Error> {
        let waited = tokio::time::timeout(self.budget, lane.acquire_owned()).await;
        let permit = match waited {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(Error::Internal(
                    "the store stopped admitting work".to_owned(),
                ))
            }
            Err(_) => {
                return Err(Error::Unavailable(
                    "the store is holding every connection it has".to_owned(),
                ))
            }
        };
        Ok(self.held(permit))
    }

    fn held(&self, permit: OwnedSemaphorePermit) -> Admission {
        let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(running, Ordering::SeqCst);
        Admission {
            _permit: permit,
            running: Arc::clone(&self.running),
        }
    }
}

impl Default for Throttle {
    fn default() -> Throttle {
        Throttle::new(16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_more_work_runs_at_once_than_the_limit_allows() {
        let throttle = Arc::new(Throttle::new(2));
        let mut running = Vec::new();
        for _ in 0..2 {
            running.push(throttle.admit().await.unwrap());
        }
        assert_eq!(throttle.running(), 2);
        assert_eq!(throttle.peak(), 2);
        let waiting = Arc::clone(&throttle);
        let queued = tokio::spawn(async move { waiting.admit().await.map(|_| ()) });
        tokio::task::yield_now().await;
        assert_eq!(throttle.running(), 2);
        running.pop();
        queued.await.unwrap().unwrap();
        assert!(throttle.peak() <= throttle.limit());
    }

    #[tokio::test]
    async fn a_place_is_given_back_when_the_work_is_over() {
        let throttle = Throttle::new(1);
        {
            let _held = throttle.admit().await.unwrap();
            assert_eq!(throttle.running(), 1);
        }
        assert_eq!(throttle.running(), 0);
        assert_eq!(throttle.limit(), 1);
    }

    #[tokio::test]
    async fn a_wait_longer_than_the_budget_is_refused_with_a_retry_hint() {
        let throttle = Throttle::new(1).waiting(std::time::Duration::from_millis(20));
        let _held = throttle.admit().await.unwrap();
        let started = std::time::Instant::now();
        let refused = throttle.admit().await;
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        assert!(matches!(refused, Err(Error::Unavailable(_))));
        assert_eq!(refused.err().and_then(|error| error.retry_after()), Some(2));
    }

    #[tokio::test]
    async fn a_cheap_read_is_not_queued_behind_stalled_callers() {
        let throttle = Throttle::new(4)
            .reserving(1)
            .waiting(std::time::Duration::from_millis(20));
        let mut stalled = Vec::new();
        for _ in 0..3 {
            stalled.push(throttle.admit().await.unwrap());
        }
        assert!(throttle.admit().await.is_err());
        assert!(throttle.admit_cheap().await.is_ok());
        drop(stalled);
    }

    #[tokio::test]
    async fn a_limit_of_nothing_still_admits_one_piece_of_work() {
        let throttle = Throttle::default();
        assert_eq!(throttle.limit(), 16);
        assert_eq!(Throttle::new(0).limit(), 1);
    }
}
