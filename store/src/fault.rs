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
            Fault::Transient => self
                .backoff
                .saturating_mul(1 << attempt.min(5).saturating_sub(1)),
            Fault::Permanent => Duration::ZERO,
        }
    }
}

pub async fn repeated<T, E, F, Fut, W, R>(
    policy: &Policy,
    context: &str,
    weigh: W,
    report: R,
    mut work: F,
) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    W: Fn(&E) -> Fault,
    R: Fn(&str, E) -> Error,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let error = match work().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let fault = weigh(&error);
        if !fault.is_retriable() || attempt >= policy.attempts {
            return Err(report(context, error));
        }
        tokio::time::sleep(policy.delay(fault, attempt)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_repeated_only_when_repeating_it_is_worth_something() {
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
}
