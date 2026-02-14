use crate::outcome::{IssueCode, OperationOutcome};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidResourceType(String),
    InvalidResourceId(String),
    InvalidVersion(String),
    InvalidEtag(String),
    InvalidFhirVersion(String),
    InvalidInstant(String),
    InvalidJson(String),
    InvalidEnvelope(String),
    Config(String),
    NotFound,
    VersionConflict,
    Duplicate(String),
    Internal(String),
    Deleted,
    MethodNotAllowed,
    MultipleMatches,
    InvalidPatch(String),
    InvalidParameter(String),
    UnsupportedParameter(String),
    Forbidden(String),
    NoMatch(String),
    Unauthenticated(String),
    Unavailable(String),
}

pub const RETRY_SECONDS: u32 = 2;

impl Error {
    pub fn retry_after(&self) -> Option<u32> {
        match self {
            Error::Unavailable(_) => Some(RETRY_SECONDS),
            _ => None,
        }
    }

    pub fn http_status(&self) -> u16 {
        self.to_operation_outcome().http_status()
    }

    pub fn to_operation_outcome(&self) -> OperationOutcome {
        match self {
            Error::InvalidResourceType(value) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid resource type: {value:?}"))
            }
            Error::InvalidResourceId(value) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid resource id: {value:?}"))
            }
            Error::InvalidVersion(value) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid version: {value:?}"))
            }
            Error::InvalidEtag(value) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid etag: {value:?}"))
            }
            Error::InvalidFhirVersion(value) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid fhir version: {value:?}"))
            }
            Error::InvalidInstant(value) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid instant: {value:?}"))
            }
            Error::InvalidJson(message) => {
                OperationOutcome::error(IssueCode::Invalid, format!("malformed json: {message}"))
            }
            Error::InvalidEnvelope(message) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid envelope: {message}"))
            }
            Error::Config(message) => {
                OperationOutcome::error(IssueCode::Processing, message.clone())
            }
            Error::NotFound => OperationOutcome::error(IssueCode::NotFound, "resource not found"),
            Error::VersionConflict => OperationOutcome::error(IssueCode::Conflict, "version conflict"),
            Error::Duplicate(value) => {
                OperationOutcome::error(IssueCode::Duplicate, format!("duplicate resource: {value:?}"))
            }
            Error::Internal(message) => {
                OperationOutcome::error(IssueCode::Processing, message.clone())
            }
            Error::Deleted => OperationOutcome::error(IssueCode::Deleted, "resource deleted"),
            Error::MethodNotAllowed => {
                OperationOutcome::error(IssueCode::NotAllowed, "method not allowed")
            }
            Error::MultipleMatches => OperationOutcome::error(
                IssueCode::MultipleMatches,
                "the conditional request matched more than one resource",
            ),
            Error::InvalidPatch(message) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid patch: {message}"))
            }
            Error::InvalidParameter(message) => {
                OperationOutcome::error(IssueCode::Invalid, format!("invalid parameter: {message}"))
            }
            Error::UnsupportedParameter(message) => OperationOutcome::error(
                IssueCode::NotSupported,
                format!("unsupported parameter: {message}"),
            ),
            Error::Forbidden(message) => {
                OperationOutcome::error(IssueCode::Forbidden, format!("out of scope: {message}"))
            }
            Error::Unauthenticated(message) => OperationOutcome::error(
                IssueCode::Login,
                format!("not authenticated: {message}"),
            ),
            Error::NoMatch(message) => {
                OperationOutcome::error(IssueCode::BusinessRule, message.clone())
            }
            Error::Unavailable(message) => OperationOutcome::error(
                IssueCode::Transient,
                format!("{message}; the request may be repeated in {RETRY_SECONDS} seconds"),
            ),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidResourceType(value) => write!(f, "invalid resource type: {value:?}"),
            Error::InvalidResourceId(value) => write!(f, "invalid resource id: {value:?}"),
            Error::InvalidVersion(value) => write!(f, "invalid version: {value:?}"),
            Error::InvalidEtag(value) => write!(f, "invalid etag: {value:?}"),
            Error::InvalidFhirVersion(value) => write!(f, "invalid fhir version: {value:?}"),
            Error::InvalidInstant(value) => write!(f, "invalid instant: {value:?}"),
            Error::InvalidJson(message) => write!(f, "malformed json: {message}"),
            Error::InvalidEnvelope(message) => write!(f, "invalid envelope: {message}"),
            Error::Config(message) => write!(f, "configuration error: {message}"),
            Error::NotFound => write!(f, "resource not found"),
            Error::VersionConflict => write!(f, "version conflict"),
            Error::Duplicate(value) => write!(f, "duplicate resource: {value:?}"),
            Error::Internal(message) => write!(f, "internal error: {message}"),
            Error::Deleted => write!(f, "resource deleted"),
            Error::MethodNotAllowed => write!(f, "method not allowed"),
            Error::MultipleMatches => write!(f, "multiple matches for the conditional request"),
            Error::InvalidPatch(message) => write!(f, "invalid patch: {message}"),
            Error::InvalidParameter(message) => write!(f, "invalid parameter: {message}"),
            Error::UnsupportedParameter(message) => write!(f, "unsupported parameter: {message}"),
            Error::Forbidden(message) => write!(f, "out of scope: {message}"),
            Error::Unauthenticated(message) => write!(f, "not authenticated: {message}"),
            Error::NoMatch(message) => write!(f, "{message}"),
            Error::Unavailable(message) => write!(f, "temporarily unavailable: {message}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueCode, ResourceId, VersionId, WeakEtag};

    #[test]
    fn invalid_input_errors_map_to_bad_request() {
        for error in [
            Error::InvalidResourceType("Nope".to_owned()),
            Error::InvalidResourceId("bad id".to_owned()),
            Error::InvalidVersion("x/y".to_owned()),
            Error::InvalidEtag("nope".to_owned()),
        ] {
            assert_eq!(error.http_status(), 400);
            assert_eq!(error.to_operation_outcome().code, IssueCode::Invalid);
        }
    }

    #[test]
    fn not_found_maps_to_404() {
        assert_eq!(Error::NotFound.http_status(), 404);
        assert_eq!(Error::NotFound.to_operation_outcome().code, IssueCode::NotFound);
    }

    #[test]
    fn version_conflict_maps_to_409() {
        assert_eq!(Error::VersionConflict.http_status(), 409);
        assert_eq!(Error::VersionConflict.to_operation_outcome().code, IssueCode::Conflict);
    }

    #[test]
    fn duplicate_maps_to_409() {
        let error = Error::Duplicate("pt-1".to_owned());
        assert_eq!(error.http_status(), 409);
        assert_eq!(error.to_operation_outcome().code, IssueCode::Duplicate);
        assert!(error.to_string().contains("pt-1"));
    }

    #[test]
    fn internal_maps_to_500() {
        let error = Error::Internal("lock poisoned".to_owned());
        assert_eq!(error.http_status(), 500);
        assert_eq!(error.to_operation_outcome().code, IssueCode::Processing);
        assert!(error.to_string().contains("lock poisoned"));
    }

    #[test]
    fn deleted_maps_to_gone() {
        assert_eq!(Error::Deleted.http_status(), 410);
        assert_eq!(Error::Deleted.to_operation_outcome().code, IssueCode::Deleted);
    }

    #[test]
    fn config_error_maps_to_internal() {
        assert_eq!(Error::Config("boom".to_owned()).http_status(), 500);
        assert_eq!(Error::Config("boom".to_owned()).to_operation_outcome().code, IssueCode::Processing);
    }

    #[test]
    fn method_not_allowed_maps_to_405() {
        assert_eq!(Error::MethodNotAllowed.http_status(), 405);
        assert_eq!(Error::MethodNotAllowed.to_operation_outcome().code, IssueCode::NotAllowed);
    }

    #[test]
    fn multiple_matches_maps_to_412() {
        let error = Error::MultipleMatches;
        assert_eq!(error.http_status(), 412);
        assert_eq!(error.to_operation_outcome().code, IssueCode::MultipleMatches);
    }

    #[test]
    fn envelope_errors_map_to_bad_request() {
        for error in [
            Error::InvalidFhirVersion("2".to_owned()),
            Error::InvalidInstant("whenever".to_owned()),
            Error::InvalidJson("unterminated string".to_owned()),
            Error::InvalidEnvelope("missing resourceType".to_owned()),
            Error::InvalidPatch("no member \"gender\"".to_owned()),
        ] {
            assert_eq!(error.http_status(), 400);
            assert_eq!(error.to_operation_outcome().code, IssueCode::Invalid);
        }
    }

    #[test]
    fn parameter_errors_map_to_bad_request() {
        let invalid = Error::InvalidParameter("_count \"many\"".to_owned());
        assert_eq!(invalid.http_status(), 400);
        assert_eq!(invalid.to_operation_outcome().code, IssueCode::Invalid);
        assert!(invalid.to_string().contains("_count"));
        let unsupported = Error::UnsupportedParameter("_sort \"name\"".to_owned());
        assert_eq!(unsupported.http_status(), 400);
        assert_eq!(unsupported.to_operation_outcome().code, IssueCode::NotSupported);
        assert!(unsupported.to_string().contains("_sort"));
    }

    #[test]
    fn issue_code_lifecycle_composes_with_newtypes() {
        let id = ResourceId::parse("err-1").unwrap();
        assert_eq!(id.as_str(), "err-1");
        let version = VersionId::parse("2").unwrap();
        assert_eq!(WeakEtag::from(&version).to_string(), "W/\"2\"");
    }

    #[test]
    fn every_failure_says_what_went_wrong_and_maps_to_a_status() {
        let held = vec![
            Error::InvalidResourceType("X".to_owned()),
            Error::InvalidResourceId("X".to_owned()),
            Error::InvalidVersion("X".to_owned()),
            Error::InvalidEtag("X".to_owned()),
            Error::InvalidFhirVersion("X".to_owned()),
            Error::InvalidInstant("X".to_owned()),
            Error::InvalidJson("X".to_owned()),
            Error::InvalidEnvelope("X".to_owned()),
            Error::Config("X".to_owned()),
            Error::NotFound,
            Error::VersionConflict,
            Error::Duplicate("X".to_owned()),
            Error::Internal("X".to_owned()),
            Error::Deleted,
            Error::MethodNotAllowed,
            Error::MultipleMatches,
            Error::InvalidPatch("X".to_owned()),
            Error::InvalidParameter("X".to_owned()),
            Error::UnsupportedParameter("X".to_owned()),
            Error::Forbidden("X".to_owned()),
            Error::NoMatch("X".to_owned()),
            Error::Unauthenticated("X".to_owned()),
            Error::Unavailable("X".to_owned()),
        ];
        for error in &held {
            assert!(!error.to_string().trim().is_empty(), "{error:?}");
            assert!((400..=599).contains(&error.http_status()), "{error:?}");
            assert!(
                !error.to_operation_outcome().to_fhir_json().is_empty(),
                "{error:?}"
            );
        }
    }
}
#[cfg(test)]
mod budget {
    use super::*;
    use crate::IssueCode;

    #[test]
    fn an_absent_dependency_is_not_a_defect_in_the_request() {
        let error = Error::Unavailable("the store is busy".to_owned());
        assert_eq!(error.http_status(), 503);
        assert_eq!(error.to_operation_outcome().code, IssueCode::Transient);
    }

    #[test]
    fn an_absent_dependency_says_when_to_come_back() {
        let error = Error::Unavailable("the store is busy".to_owned());
        assert_eq!(error.retry_after(), Some(RETRY_SECONDS));
        assert_eq!(Error::NotFound.retry_after(), None);
    }
}
