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

    #[test]
    fn a_failure_the_engine_names_is_placed_by_its_code() {
        assert_eq!(coded(112), Fault::Transient);
        assert_eq!(coded(189), Fault::Transient);
        assert_eq!(coded(16500), Fault::Throttled);
        assert_eq!(coded(DUPLICATE), Fault::Permanent);
        assert_eq!(coded(2), Fault::Permanent);
    }
}
