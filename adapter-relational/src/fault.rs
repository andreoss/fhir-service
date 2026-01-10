use fhir_core::Error;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Transient,
    Throttled,
    Permanent,
}

impl Fault {
    pub fn is_retriable(&self) -> bool {
        !matches!(self, Fault::Permanent)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Fault::Transient => "transient",
            Fault::Throttled => "throttled",
            Fault::Permanent => "permanent",
        }
    }
}

const TRANSIENT: [&str; 6] = ["40001", "40P01", "57P03", "55P03", "57014", "58030"];
const THROTTLED: [&str; 4] = ["53100", "53200", "53300", "53400"];

pub fn classify(error: &sqlx::Error) -> Fault {
    match error {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::WorkerCrashed => {
            Fault::Transient
        }
        sqlx::Error::Database(reported) => match reported.code() {
            None => Fault::Permanent,
            Some(code) => coded(&code),
        },
        _ => Fault::Permanent,
    }
}

fn coded(code: &str) -> Fault {
    if TRANSIENT.contains(&code) {
        return Fault::Transient;
    }
    if THROTTLED.contains(&code) {
        return Fault::Throttled;
    }
    match code.starts_with("08") {
        true => Fault::Transient,
        false => Fault::Permanent,
    }
}

pub fn classified(context: &str, error: sqlx::Error) -> Error {
    if let sqlx::Error::Database(reported) = &error {
        if reported.code().as_deref() == Some("23505") {
            return Error::Duplicate(format!("{context}: the value is already held"));
        }
    }
    Error::Internal(format!("{context}: {error}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub attempts: u32,
    pub backoff: Duration,
    pub pause: Duration,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            attempts: 4,
            backoff: Duration::from_millis(20),
            pause: Duration::from_millis(50),
        }
    }
}

impl Policy {
    pub fn once() -> Policy {
        Policy {
            attempts: 1,
            ..Policy::default()
        }
    }

    pub fn delay(&self, fault: Fault, attempt: u32) -> Duration {
        match fault {
            Fault::Throttled => self.pause.saturating_mul(attempt),
            Fault::Transient => self.backoff.saturating_mul(1 << attempt.min(5).saturating_sub(1)),
            Fault::Permanent => Duration::ZERO,
        }
    }
}

pub async fn retried<T, F, Fut>(policy: &Policy, context: &str, mut work: F) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let error = match work().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let fault = classify(&error);
        if !fault.is_retriable() || attempt >= policy.attempts {
            return Err(classified(context, error));
        }
        tokio::time::sleep(policy.delay(fault, attempt)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn transient() -> sqlx::Error {
        sqlx::Error::PoolTimedOut
    }

    fn permanent() -> sqlx::Error {
        sqlx::Error::RowNotFound
    }

    fn quick() -> Policy {
        Policy {
            attempts: 4,
            backoff: Duration::from_micros(1),
            pause: Duration::from_micros(1),
        }
    }

    #[test]
    fn a_failure_the_engine_names_is_placed_by_its_code() {
        assert_eq!(coded("40001"), Fault::Transient);
        assert_eq!(coded("40P01"), Fault::Transient);
        assert_eq!(coded("08006"), Fault::Transient);
        assert_eq!(coded("53300"), Fault::Throttled);
        assert_eq!(coded("23505"), Fault::Permanent);
        assert_eq!(coded("42601"), Fault::Permanent);
    }

    #[test]
    fn a_dropped_connection_is_worth_another_attempt() {
        assert_eq!(classify(&transient()), Fault::Transient);
        assert_eq!(classify(&permanent()), Fault::Permanent);
        assert!(Fault::Transient.is_retriable());
        assert!(Fault::Throttled.is_retriable());
        assert!(!Fault::Permanent.is_retriable());
        assert_eq!(Fault::Throttled.as_str(), "throttled");
        assert_eq!(Fault::Transient.as_str(), "transient");
        assert_eq!(Fault::Permanent.as_str(), "permanent");
    }

    #[test]
    fn a_throttled_attempt_waits_longer_the_more_it_is_repeated() {
        let policy = Policy::default();
        assert!(policy.delay(Fault::Throttled, 2) > policy.delay(Fault::Throttled, 1));
        assert!(policy.delay(Fault::Transient, 3) > policy.delay(Fault::Transient, 1));
        assert_eq!(policy.delay(Fault::Permanent, 3), Duration::ZERO);
        assert_eq!(Policy::once().attempts, 1);
    }

    #[tokio::test]
    async fn work_that_recovers_is_repeated_until_it_does() {
        let seen = AtomicU32::new(0);
        let value = retried(&quick(), "reading", || async {
            match seen.fetch_add(1, Ordering::SeqCst) {
                0 | 1 => Err(transient()),
                _ => Ok(7),
            }
        })
        .await
        .unwrap();
        assert_eq!(value, 7);
        assert_eq!(seen.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn work_that_will_never_succeed_is_not_repeated() {
        let seen = AtomicU32::new(0);
        let failure = retried(&quick(), "reading", || async {
            seen.fetch_add(1, Ordering::SeqCst);
            Err::<u32, sqlx::Error>(permanent())
        })
        .await;
        assert!(failure.is_err());
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn work_that_keeps_failing_gives_up_after_the_last_attempt() {
        let seen = AtomicU32::new(0);
        let failure = retried(&quick(), "reading", || async {
            seen.fetch_add(1, Ordering::SeqCst);
            Err::<u32, sqlx::Error>(transient())
        })
        .await;
        assert!(matches!(failure, Err(Error::Internal(_))));
        assert_eq!(seen.load(Ordering::SeqCst), 4);
    }
}
