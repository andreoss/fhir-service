use crate::ResourceId;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSeverity {
    Fatal,
    Error,
    Warning,
    Information,
}

impl fmt::Display for IssueSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            IssueSeverity::Fatal => "fatal",
            IssueSeverity::Error => "error",
            IssueSeverity::Warning => "warning",
            IssueSeverity::Information => "information",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueCode {
    Invalid,
    NotFound,
    Conflict,
    StaleVersion,
    Deleted,
    Forbidden,
    Login,
    NotSupported,
    NotAcceptable,
    NotAllowed,
    Duplicate,
    MultipleMatches,
    Processing,
    Informational,
    BusinessRule,
    Transient,
    Throttled,
    TooLarge,
}

impl IssueCode {
    pub fn as_str(&self) -> &str {
        match self {
            IssueCode::Invalid => "invalid",
            IssueCode::NotFound => "not-found",
            IssueCode::Conflict => "conflict",
            IssueCode::StaleVersion => "conflict",
            IssueCode::Deleted => "deleted",
            IssueCode::Forbidden => "forbidden",
            IssueCode::Login => "login",
            IssueCode::NotSupported => "not-supported",
            IssueCode::NotAcceptable => "not-supported",
            IssueCode::NotAllowed => "forbidden",
            IssueCode::Duplicate => "duplicate",
            IssueCode::MultipleMatches => "multiple-matches",
            IssueCode::Processing => "processing",
            IssueCode::Informational => "informational",
            IssueCode::BusinessRule => "business-rule",
            IssueCode::Transient => "transient",
            IssueCode::Throttled => "throttled",
            IssueCode::TooLarge => "too-costly",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            IssueCode::Invalid | IssueCode::NotSupported => 400,
            IssueCode::NotFound => 404,
            IssueCode::Deleted => 410,
            IssueCode::Conflict | IssueCode::Duplicate => 409,
            IssueCode::NotAcceptable => 406,
            IssueCode::StaleVersion => 412,
            IssueCode::Forbidden => 403,
            IssueCode::Login => 401,
            IssueCode::NotAllowed => 405,
            IssueCode::MultipleMatches => 412,
            IssueCode::Processing => 500,
            IssueCode::Informational => 200,
            IssueCode::BusinessRule => 422,
            IssueCode::Transient => 503,
            IssueCode::Throttled => 429,
            IssueCode::TooLarge => 413,
        }
    }
}

impl fmt::Display for IssueCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationOutcome {
    pub id: Option<ResourceId>,
    pub severity: IssueSeverity,
    pub code: IssueCode,
    pub diagnostics: Option<String>,
}

impl OperationOutcome {
    pub fn error(code: IssueCode, diagnostics: impl Into<String>) -> OperationOutcome {
        OperationOutcome {
            id: None,
            severity: IssueSeverity::Error,
            code,
            diagnostics: Some(diagnostics.into()),
        }
    }

    
    pub fn information(diagnostics: impl Into<String>) -> OperationOutcome {
        OperationOutcome {
            id: None,
            severity: IssueSeverity::Information,
            code: IssueCode::Informational,
            diagnostics: Some(diagnostics.into()),
        }
    }

    pub fn http_status(&self) -> u16 {
        self.code.http_status()
    }

    pub fn to_fhir_json(&self) -> Vec<u8> {
        let mut issue = serde_json::Map::new();
        issue.insert(
            "severity".to_owned(),
            serde_json::Value::String(self.severity.to_string()),
        );
        issue.insert(
            "code".to_owned(),
            serde_json::Value::String(self.code.to_string()),
        );
        if let Some(diagnostics) = &self.diagnostics {
            issue.insert(
                "diagnostics".to_owned(),
                serde_json::Value::String(diagnostics.clone()),
            );
        }
        let mut body = serde_json::Map::new();
        body.insert(
            "resourceType".to_owned(),
            serde_json::Value::String("OperationOutcome".to_owned()),
        );
        if let Some(id) = &self.id {
            body.insert(
                "id".to_owned(),
                serde_json::Value::String(id.as_str().to_owned()),
            );
        }
        body.insert(
            "issue".to_owned(),
            serde_json::Value::Array(vec![serde_json::Value::Object(issue)]),
        );
        serde_json::to_vec(&serde_json::Value::Object(body))
            .expect("operation outcome is serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_codes_match_fhir_fhir_strings() {
        assert_eq!(IssueSeverity::Fatal.to_string(), "fatal");
        assert_eq!(IssueSeverity::Error.to_string(), "error");
        assert_eq!(IssueSeverity::Warning.to_string(), "warning");
        assert_eq!(IssueSeverity::Information.to_string(), "information");
    }

    const EVERY_CODE: [IssueCode; 16] = [
        IssueCode::Invalid,
        IssueCode::NotFound,
        IssueCode::Conflict,
        IssueCode::StaleVersion,
        IssueCode::Deleted,
        IssueCode::Forbidden,
        IssueCode::Login,
        IssueCode::NotSupported,
        IssueCode::NotAcceptable,
        IssueCode::NotAllowed,
        IssueCode::Duplicate,
        IssueCode::MultipleMatches,
        IssueCode::Processing,
        IssueCode::Informational,
        IssueCode::BusinessRule,
        IssueCode::Transient,
    ];

    #[test]
    fn issue_codes_match_fhir_strings() {
        assert_eq!(IssueCode::Invalid.as_str(), "invalid");
        assert_eq!(IssueCode::NotFound.as_str(), "not-found");
        assert_eq!(IssueCode::Conflict.as_str(), "conflict");
        assert_eq!(IssueCode::Processing.as_str(), "processing");
    }

    #[test]
    fn every_issue_code_is_one_the_published_value_set_carries() {
        let published = [
            "business-rule",
            "code-invalid",
            "conflict",
            "deleted",
            "duplicate",
            "exception",
            "expired",
            "extension",
            "forbidden",
            "incomplete",
            "informational",
            "invalid",
            "invariant",
            "lock-error",
            "login",
            "multiple-matches",
            "no-store",
            "not-found",
            "not-supported",
            "processing",
            "required",
            "security",
            "structure",
            "suppressed",
            "throttled",
            "timeout",
            "too-costly",
            "too-long",
            "transient",
            "unknown",
            "value",
        ];
        for code in EVERY_CODE {
            assert!(
                published.contains(&code.as_str()),
                "{code:?} renders {:?}, which issue-type does not carry",
                code.as_str()
            );
        }
    }

    #[test]
    fn issue_codes_map_to_http_status() {
        let expected = [
            (IssueCode::Invalid, 400),
            (IssueCode::NotSupported, 400),
            (IssueCode::NotAcceptable, 406),
            (IssueCode::Login, 401),
            (IssueCode::Forbidden, 403),
            (IssueCode::NotFound, 404),
            (IssueCode::NotAllowed, 405),
            (IssueCode::Conflict, 409),
            (IssueCode::Duplicate, 409),
            (IssueCode::Deleted, 410),
            (IssueCode::MultipleMatches, 412),
            (IssueCode::StaleVersion, 412),
            (IssueCode::BusinessRule, 422),
            (IssueCode::Processing, 500),
            (IssueCode::Transient, 503),
            (IssueCode::Informational, 200),
        ];
        assert_eq!(expected.len(), EVERY_CODE.len());
        for (code, status) in expected {
            assert_eq!(code.http_status(), status, "{code:?}");
        }
    }

    #[test]
    fn error_outcome_has_severity_and_status() {
        let outcome = OperationOutcome::error(IssueCode::NotFound, "resource not found");
        assert_eq!(outcome.severity, IssueSeverity::Error);
        assert_eq!(outcome.code, IssueCode::NotFound);
        assert_eq!(outcome.http_status(), 404);
    }

    #[test]
    fn to_fhir_json_renders_fhir_shape() {
        let outcome = OperationOutcome::error(IssueCode::NotFound, "resource not found");
        let text = String::from_utf8(outcome.to_fhir_json()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["resourceType"], "OperationOutcome");
        assert_eq!(value["issue"][0]["severity"], "error");
        assert_eq!(value["issue"][0]["code"], "not-found");
        assert_eq!(value["issue"][0]["diagnostics"], "resource not found");
    }

    #[test]
    fn to_fhir_json_omits_missing_diagnostics() {
        let outcome = OperationOutcome {
            id: None,
            severity: IssueSeverity::Warning,
            code: IssueCode::Conflict,
            diagnostics: None,
        };
        let text = String::from_utf8(outcome.to_fhir_json()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["issue"][0]["severity"], "warning");
        assert_eq!(value["issue"][0]["code"], "conflict");
        assert!(value["issue"][0].get("diagnostics").is_none());
    }
}
