use fhir_core::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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
    limit: usize,
    running: Arc<AtomicUsize>,
    peak: AtomicUsize,
}

impl Throttle {
    pub fn new(limit: usize) -> Throttle {
        let limit = limit.max(1);
        Throttle {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
            running: Arc::new(AtomicUsize::new(0)),
            peak: AtomicUsize::new(0),
        }
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
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| Error::Internal("the store stopped admitting work".to_owned()))?;
        let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(running, Ordering::SeqCst);
        Ok(Admission {
            _permit: permit,
            running: Arc::clone(&self.running),
        })
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
    async fn a_limit_of_nothing_still_admits_one_piece_of_work() {
        let throttle = Throttle::default();
        assert_eq!(throttle.limit(), 16);
        assert_eq!(Throttle::new(0).limit(), 1);
    }
}
