use fhir_core::Error;
pub use fhir_store::fault::{Fault, Policy};
use mongodb::error::{Error as DriverError, ErrorKind};

const DUPLICATE: i32 = 11000;
const TRANSIENT: [i32; 6] = [112, 117, 189, 91, 262, 24];
const THROTTLED: [i32; 3] = [16500, 50, 82];

pub fn classify(error: &DriverError) -> Fault {
    if error.contains_label("TransientTransactionError")
        || error.contains_label("UnknownTransactionCommitResult")
    {
        return Fault::Transient;
    }
    match error.kind.as_ref() {
        ErrorKind::Io(_) | ErrorKind::ConnectionPoolCleared { .. } => Fault::Transient,
        ErrorKind::ServerSelection { .. } => Fault::Transient,
        _ => match code_of(error) {
            Some(code) => coded(code),
            None => Fault::Permanent,
        },
    }
}

fn coded(code: i32) -> Fault {
    if TRANSIENT.contains(&code) {
        return Fault::Transient;
    }
    match THROTTLED.contains(&code) {
        true => Fault::Throttled,
        false => Fault::Permanent,
    }
}

fn code_of(error: &DriverError) -> Option<i32> {
    match error.kind.as_ref() {
        ErrorKind::Command(reported) => Some(reported.code),
        ErrorKind::Write(failure) => match failure {
            mongodb::error::WriteFailure::WriteError(reported) => Some(reported.code),
            mongodb::error::WriteFailure::WriteConcernError(reported) => Some(reported.code),
            _ => None,
        },
        ErrorKind::BulkWrite(failure) => failure
            .write_errors
            .values()
            .next()
            .map(|reported| reported.code),
        _ => None,
    }
}

pub fn classified(context: &str, error: DriverError) -> Error {
    match code_of(&error) {
        Some(DUPLICATE) => Error::Duplicate(format!("{context}: the value is already held")),
        _ => Error::Internal(format!("{context}: {error}")),
    }
}

pub async fn retried<T, F, Fut>(policy: &Policy, context: &str, work: F) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, DriverError>>,
{
    fhir_store::fault::repeated(policy, context, classify, classified, work).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn transient() -> DriverError {
        DriverError::from(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "the connection dropped",
        ))
    }

    fn permanent() -> DriverError {
        DriverError::from(mongodb::bson::de::Error::EndOfStream)
    }

    fn quick() -> Policy {
        Policy {
            attempts: 4,
            backoff: Duration::from_micros(1),
            pause: Duration::from_micros(1),
        }
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
            Err::<u32, DriverError>(permanent())
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
            Err::<u32, DriverError>(transient())
        })
        .await;
        assert!(matches!(failure, Err(Error::Internal(_))));
        assert_eq!(seen.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_failure_the_engine_names_is_placed_by_its_code() {
        assert_eq!(coded(112), Fault::Transient);
        assert_eq!(coded(189), Fault::Transient);
        assert_eq!(coded(16500), Fault::Throttled);
        assert_eq!(coded(DUPLICATE), Fault::Permanent);
        assert_eq!(coded(2), Fault::Permanent);
    }
}
