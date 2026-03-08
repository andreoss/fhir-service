use crate::model::{Model, Rule};
use crate::outcome::{IssueCode, IssueSeverity};
use crate::{Error, FhirVersion, ResourceId, ResourceType};
use serde_json::{Map, Value};
use std::str::FromStr;

const NARRATIVE_STATUS: [&str; 4] = ["generated", "extensions", "additional", "empty"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Create,
    Update,
    Delete,
}

impl FromStr for Mode {
    type Err = Error;

    fn from_str(text: &str) -> Result<Mode, Error> {
        match text {
            "create" => Ok(Mode::Create),
            "update" => Ok(Mode::Update),
            "delete" => Ok(Mode::Delete),
            other => Err(Error::UnsupportedParameter(format!("mode {other:?}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: IssueSeverity,
    pub code: IssueCode,
    pub diagnostics: String,
    pub expression: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    issues: Vec<Issue>,
}

impl Report {
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| matches!(issue.severity, IssueSeverity::Error | IssueSeverity::Fatal))
    }

    pub fn to_fhir_json(&self) -> Vec<u8> {
        let issues: Vec<Value> = self
            .issues
            .iter()
            .map(|issue| {
                let mut held = Map::new();
                held.insert(
                    "severity".to_owned(),
                    Value::String(issue.severity.to_string()),
                );
                held.insert("code".to_owned(), Value::String(issue.code.to_string()));
                held.insert(
                    "diagnostics".to_owned(),
                    Value::String(issue.diagnostics.clone()),
                );
                if let Some(expression) = &issue.expression {
                    held.insert(
                        "expression".to_owned(),
                        Value::Array(vec![Value::String(expression.clone())]),
                    );
                }
                Value::Object(held)
            })
            .collect();
        let body = serde_json::json!({
            "resourceType": "OperationOutcome",
            "issue": issues,
        });
        serde_json::to_vec(&body).expect("operation outcome is serializable")
    }

    pub fn to_fhir_json_text(&self) -> String {
        String::from_utf8_lossy(&self.to_fhir_json()).into_owned()
    }
}




pub struct Resolved<'a> {
    pub profile: &'a crate::profile::Profile,
    pub codes: &'a dyn crate::profile::CodeSource,
}

pub struct Request<'a> {
    pub version: FhirVersion,
    pub resource_type: Option<ResourceType>,
    pub id: Option<ResourceId>,
    pub profile: Option<&'a str>,
    
    
    pub resolved: Option<Resolved<'a>>,
    pub mode: Mode,
    pub body: &'a Value,
}

pub fn validate(request: &Request) -> Report {
    let mut issues = Vec::new();
    if request.mode == Mode::Delete && request.body.is_null() {
        return informational(issues);
    }
    let Some(object) = request.body.as_object() else {
        issues.push(error("the body is not a resource", None));
        return Report { issues };
    };
    structure(object, request, &mut issues);
    definitions(request, &mut issues);
    profile(object, request, &mut issues);
    narrative(object, &mut issues);
    empty_elements(object, String::new(), &mut issues);
    informational(issues)
}

fn definitions(request: &Request, issues: &mut Vec<Issue>) {
    for finding in Model::of(request.version).check(request.body) {
        issues.push(Issue {
            severity: IssueSeverity::Error,
            code: IssueCode::Invalid,
            diagnostics: format!("{}: {}", finding.rule, finding.detail),
            expression: Some(finding.path),
        });
    }
}

fn informational(issues: Vec<Issue>) -> Report {
    let mut issues = issues;
    if issues.is_empty() {
        issues.push(Issue {
            severity: IssueSeverity::Information,
            code: IssueCode::Informational,
            diagnostics: "the resource is valid".to_owned(),
            expression: None,
        });
    }
    Report { issues }
}

fn structure(object: &Map<String, Value>, request: &Request, issues: &mut Vec<Issue>) {
    let held = match object.get("resourceType").and_then(Value::as_str) {
        None => {
            issues.push(error(
                "the body carries no resourceType",
                Some("resourceType"),
            ));
            return;
        }
        Some(text) => text,
    };
    let parsed = match held.parse::<ResourceType>() {
        Ok(parsed) => parsed,
        Err(_) => {
            issues.push(error(
                &format!("resourceType {held:?} is not a resource type"),
                Some("resourceType"),
            ));
            return;
        }
    };
    if let Some(wanted) = request.resource_type {
        if wanted != parsed {
            issues.push(error(
                &format!(
                    "the body is a {held:?} where a {:?} was addressed",
                    wanted.as_str()
                ),
                Some("resourceType"),
            ));
        }
    }
    match (object.get("id").and_then(Value::as_str), &request.id) {
        (Some(held), Some(wanted)) if held != wanted.as_str() => issues.push(error(
            &format!(
                "the body carries id {held:?} where {:?} was addressed",
                wanted.as_str()
            ),
            Some("id"),
        )),
        (None, Some(wanted)) => issues.push(error(
            &format!(
                "the body carries no id where {:?} was addressed",
                wanted.as_str()
            ),
            Some("id"),
        )),
        (Some(held), None) if request.mode == Mode::Create => issues.push(Issue {
            severity: IssueSeverity::Warning,
            code: IssueCode::Invalid,
            diagnostics: format!("a created resource carries id {held:?}"),
            expression: Some("id".to_owned()),
        }),
        _ => {}
    }
}

fn profile(object: &Map<String, Value>, request: &Request, issues: &mut Vec<Issue>) {
    let Some(wanted) = request.profile else {
        return;
    };
    let claimed = object
        .get("meta")
        .and_then(|meta| meta.get("profile"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .any(|held| held == wanted)
        })
        .unwrap_or(false);
    if !claimed {
        issues.push(error(
            &format!(
                "{}: the resource does not claim profile {wanted:?}",
                Rule::Profile
            ),
            Some("meta.profile"),
        ));
        return;
    }
    let model = Model::of(request.version);
    let held = object.get("resourceType").and_then(Value::as_str);
    if let Some(resolved) = &request.resolved {
        match held {
            Some(kind) if kind != resolved.profile.base_type() => {
                issues.push(error(
                    &format!(
                        "{}: profile {wanted:?} constrains a {}, not a {kind}",
                        Rule::Profile,
                        resolved.profile.base_type()
                    ),
                    Some("meta.profile"),
                ));
                return;
            }
            _ => {}
        }
        issues.extend(resolved.profile.judge(request.body, resolved.codes));
        return;
    }
    match (model.profiled(wanted), held) {
        (Some(named), Some(held)) if named != held => issues.push(error(
            &format!(
                "{}: profile {wanted:?} constrains a {named}, not a {held}",
                Rule::Profile
            ),
            Some("meta.profile"),
        )),
        
        
        (Some(named), Some(held)) if base_profile(wanted, named) && named == held => {}
        (_, _) => issues.push(error(
            &format!(
                "{}: profile {wanted:?} could not be resolved, so its rules cannot be applied; \
                 supply its StructureDefinition or do not claim it",
                Rule::Profile
            ),
            Some("meta.profile"),
        )),
    }
}



fn base_profile(url: &str, named: &str) -> bool {
    url.split('|')
        .next()
        .and_then(|base| base.rsplit('/').next())
        .is_some_and(|tail| tail == named)
        && url.starts_with("http://hl7.org/fhir/StructureDefinition/")
}

fn narrative(object: &Map<String, Value>, issues: &mut Vec<Issue>) {
    let Some(text) = object.get("text") else {
        return;
    };
    match text.get("status").and_then(Value::as_str) {
        Some(status) if NARRATIVE_STATUS.contains(&status) => {}
        Some(status) => issues.push(error(
            &format!(
                "{}: status {status:?} is not a narrative status",
                Rule::Narrative
            ),
            Some("text.status"),
        )),
        None => issues.push(error(
            &format!("{}: the narrative carries no status", Rule::Narrative),
            Some("text.status"),
        )),
    }
    match text.get("div").and_then(Value::as_str) {
        Some(div) if div.trim_start().starts_with("<div") && div.trim_end().ends_with("</div>") => {
        }
        Some(_) => issues.push(error(
            &format!(
                "{}: the narrative is not an xhtml division",
                Rule::Narrative
            ),
            Some("text.div"),
        )),
        None => issues.push(error(
            &format!("{}: the narrative carries no text", Rule::Narrative),
            Some("text.div"),
        )),
    }
}

fn empty_elements(object: &Map<String, Value>, prefix: String, issues: &mut Vec<Issue>) {
    for (name, value) in object {
        let path = match prefix.is_empty() {
            true => name.clone(),
            false => format!("{prefix}.{name}"),
        };
        match value {
            Value::Null => issues.push(error(
                &format!("element {path:?} carries no value"),
                Some(&path),
            )),
            Value::Array(items) if items.is_empty() => issues.push(error(
                &format!("element {path:?} carries no value"),
                Some(&path),
            )),
            Value::Array(items) => {
                for item in items {
                    if let Some(nested) = item.as_object() {
                        empty_elements(nested, path.clone(), issues);
                    }
                }
            }
            Value::Object(nested) => empty_elements(nested, path, issues),
            _ => {}
        }
    }
}

fn error(diagnostics: &str, expression: Option<&str>) -> Issue {
    Issue {
        severity: IssueSeverity::Error,
        code: IssueCode::Invalid,
        diagnostics: diagnostics.to_owned(),
        expression: expression.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_resource_type_is_reported() {
        let body = serde_json::json!({"resourceType": "Nonesuch"});
        let report = validate(&Request {
            version: FhirVersion::R4,
            resource_type: None,
            id: None,
            profile: None,
            resolved: None,
            mode: Mode::Update,
            body: &body,
        });
        assert!(report.has_errors());
    }

    #[test]
    fn a_body_that_is_not_an_object_is_reported() {
        let body = serde_json::json!("text");
        let report = validate(&Request {
            version: FhirVersion::R4,
            resource_type: None,
            id: None,
            profile: None,
            resolved: None,
            mode: Mode::Create,
            body: &body,
        });
        assert!(report.has_errors());
        assert_eq!(report.issues().len(), 1);
    }

    #[test]
    fn an_addressed_id_the_body_lacks_is_reported() {
        let body = serde_json::json!({"resourceType": "Patient"});
        let report = validate(&Request {
            version: FhirVersion::R4,
            resource_type: None,
            id: Some("pt-1".parse().unwrap()),
            profile: None,
            resolved: None,
            mode: Mode::Update,
            body: &body,
        });
        assert!(report.has_errors());
    }

    #[test]
    fn a_nested_empty_element_carries_its_path() {
        let body = serde_json::json!({
            "resourceType": "Patient",
            "name": [{"given": []}]
        });
        let report = validate(&Request {
            version: FhirVersion::R4,
            resource_type: None,
            id: None,
            profile: None,
            resolved: None,
            mode: Mode::Update,
            body: &body,
        });
        assert!(report.to_fhir_json_text().contains("name.given"));
    }
}
