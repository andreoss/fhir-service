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
    Deleted,
    Forbidden,
    NotSupported,
    NotAllowed,
    Duplicate,
    Processing,
}

impl IssueCode {
    pub fn as_str(&self) -> &str {
        match self {
            IssueCode::Invalid => "invalid",
            IssueCode::NotFound => "not-found",
            IssueCode::Conflict => "conflict",
            IssueCode::Deleted => "deleted",
            IssueCode::Forbidden => "forbidden",
            IssueCode::NotSupported => "not-supported",
            IssueCode::NotAllowed => "not-allowed",
            IssueCode::Duplicate => "duplicate",
            IssueCode::Processing => "processing",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            IssueCode::Invalid | IssueCode::NotSupported => 400,
            IssueCode::NotFound | IssueCode::Deleted => 404,
            IssueCode::Conflict | IssueCode::Duplicate => 409,
            IssueCode::Forbidden => 403,
            IssueCode::NotAllowed => 405,
            IssueCode::Processing => 500,
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

    pub fn http_status(&self) -> u16 {
        self.code.http_status()
    }

    pub fn to_fhir_json(&self) -> Vec<u8> {
        let mut issue = serde_json::Map::new();
        issue.insert("severity".to_owned(), serde_json::Value::String(self.severity.to_string()));
        issue.insert("code".to_owned(), serde_json::Value::String(self.code.to_string()));
        if let Some(diagnostics) = &self.diagnostics {
            issue.insert("diagnostics".to_owned(), serde_json::Value::String(diagnostics.clone()));
        }
        let mut body = serde_json::Map::new();
        body.insert("resourceType".to_owned(), serde_json::Value::String("OperationOutcome".to_owned()));
        if let Some(id) = &self.id {
            body.insert("id".to_owned(), serde_json::Value::String(id.as_str().to_owned()));
        }
        body.insert("issue".to_owned(), serde_json::Value::Array(vec![serde_json::Value::Object(issue)]));
        serde_json::to_vec(&serde_json::Value::Object(body)).expect("operation outcome is serializable")
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

    #[test]
    fn issue_codes_match_fhir_strings() {
        assert_eq!(IssueCode::Invalid.as_str(), "invalid");
        assert_eq!(IssueCode::NotFound.as_str(), "not-found");
        assert_eq!(IssueCode::Conflict.as_str(), "conflict");
        assert_eq!(IssueCode::Processing.as_str(), "processing");
    }

    #[test]
    fn issue_codes_map_to_http_status() {
        assert_eq!(IssueCode::Invalid.http_status(), 400);
        assert_eq!(IssueCode::NotSupported.http_status(), 400);
        assert_eq!(IssueCode::NotFound.http_status(), 404);
        assert_eq!(IssueCode::Deleted.http_status(), 404);
        assert_eq!(IssueCode::Conflict.http_status(), 409);
        assert_eq!(IssueCode::Forbidden.http_status(), 403);
        assert_eq!(IssueCode::Processing.http_status(), 500);
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