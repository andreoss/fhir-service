use fhir_core::Error;
pub use fhir_store::fault::{Fault, Policy};

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
    if classify(&error) == Fault::Transient {
        return Error::Unavailable(format!("{context}: the store is not answering"));
    }
    Error::Internal(format!("{context}: {error}"))
}

pub async fn retried<T, F, Fut>(policy: &Policy, context: &str, work: F) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    fhir_store::fault::repeated(policy, context, classify, classified, work).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

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
        assert!(matches!(failure, Err(Error::Unavailable(_))));
        assert_eq!(seen.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_dependency_that_is_absent_is_not_a_defect_in_the_request() {
        let refused = classified("starting a write", sqlx::Error::PoolTimedOut);
        assert!(matches!(refused, Error::Unavailable(_)));
        assert_eq!(refused.http_status(), 503);
        assert_eq!(refused.retry_after(), Some(2));
    }

    #[test]
    fn a_statement_the_engine_refuses_stays_a_defect_in_the_request() {
        let refused = classified("reading", sqlx::Error::RowNotFound);
        assert!(matches!(refused, Error::Internal(_)));
        assert_eq!(refused.retry_after(), None);
    }
}
