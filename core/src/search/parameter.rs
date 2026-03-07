use crate::search::registry::{ParamDef, Target};
use crate::search::value::ValueType;
use crate::{Error, ResourceType};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterSpec {
    pub url: String,
    pub base: Vec<ResourceType>,
    pub def: Arc<ParamDef>,
    pub retired: bool,
}

impl ParameterSpec {
    pub fn parse(body: &Value) -> Result<ParameterSpec, Error> {
        if body.get("resourceType").and_then(Value::as_str) != Some("SearchParameter") {
            return Err(Error::InvalidParameter(
                "definition is not a search parameter".to_owned(),
            ));
        }
        let url = text(body, "url")?;
        let code = text(body, "code")?;
        if code.starts_with('_') {
            return Err(Error::InvalidParameter(format!(
                "code {code:?} is reserved for the server"
            )));
        }
        let value_type = value_type(&text(body, "type")?)?;
        let base = base(body)?;
        let paths = expression(&text(body, "expression")?, &base)?;
        let targets = targets(body)?;
        let retired = matches!(
            body.get("status").and_then(Value::as_str),
            Some("retired") | Some("unknown")
        );
        Ok(ParameterSpec {
            def: Arc::new(ParamDef {
                name: code,
                value_type,
                target: Target::Path(paths),
                targets,
                sortable: false,
                url: Some(url.clone()),
            }),
            url,
            base,
            retired,
        })
    }

    pub fn code(&self) -> &str {
        &self.def.name
    }
}

fn text(body: &Value, name: &str) -> Result<String, Error> {
    match body.get(name).and_then(Value::as_str) {
        Some(found) if !found.is_empty() => Ok(found.to_owned()),
        _ => Err(Error::InvalidParameter(format!(
            "definition needs a {name}"
        ))),
    }
}

fn value_type(raw: &str) -> Result<ValueType, Error> {
    match raw {
        "number" => Ok(ValueType::Number),
        "date" => Ok(ValueType::Date),
        "string" => Ok(ValueType::String),
        "token" => Ok(ValueType::Token),
        "reference" => Ok(ValueType::Reference),
        "quantity" => Ok(ValueType::Quantity),
        "uri" => Ok(ValueType::Uri),
        other => Err(Error::UnsupportedParameter(format!(
            "parameter type {other:?}"
        ))),
    }
}

fn base(body: &Value) -> Result<Vec<ResourceType>, Error> {
    let names: Vec<&str> = match body.get("base") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(one)) => vec![one.as_str()],
        _ => Vec::new(),
    };
    if names.is_empty() {
        return Err(Error::InvalidParameter(
            "definition needs a base type".to_owned(),
        ));
    }
    names
        .into_iter()
        .map(str::parse::<ResourceType>)
        .collect::<Result<Vec<ResourceType>, Error>>()
}

fn targets(body: &Value) -> Result<Vec<String>, Error> {
    let Some(Value::Array(items)) = body.get("target") else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .map(|name| {
            name.parse::<ResourceType>()
                .map(|kind| kind.as_str().to_owned())
        })
        .collect()
}

fn expression(raw: &str, base: &[ResourceType]) -> Result<Vec<String>, Error> {
    let mut paths = Vec::new();
    for part in raw
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if part.contains(['(', ')', ' ', '\'']) {
            return Err(Error::UnsupportedParameter(format!(
                "expression {part:?} is not a path"
            )));
        }
        let path = base
            .iter()
            .find_map(|kind| part.strip_prefix(&format!("{}.", kind.as_str())))
            .unwrap_or(part);
        if path.is_empty() {
            return Err(Error::InvalidParameter(format!("expression {part:?}")));
        }
        paths.push(path.to_owned());
    }
    if paths.is_empty() {
        return Err(Error::InvalidParameter(
            "definition needs an expression".to_owned(),
        ));
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Value {
        json!({
            "resourceType": "SearchParameter",
            "id": "sp-1",
            "url": "urn:p:risk-band",
            "status": "active",
            "code": "risk-band",
            "base": ["Patient"],
            "type": "token",
            "expression": "Patient.extension.valueCode"
        })
    }

    #[test]
    fn a_definition_becomes_the_parameter_its_members_denote() {
        let spec = ParameterSpec::parse(&body()).unwrap();
        assert_eq!(spec.url, "urn:p:risk-band");
        assert_eq!(spec.code(), "risk-band");
        assert_eq!(spec.def.value_type, ValueType::Token);
        assert_eq!(spec.def.paths(), vec!["extension.valueCode".to_owned()]);
        assert_eq!(spec.base, vec!["Patient".parse::<ResourceType>().unwrap()]);
        assert_eq!(spec.def.url.as_deref(), Some("urn:p:risk-band"));
        assert!(!spec.retired);
        assert!(!spec.def.sortable);
    }

    #[test]
    fn every_kind_the_specification_publishes_is_read_and_composite_is_refused() {
        for (code, wanted) in [
            ("number", ValueType::Number),
            ("date", ValueType::Date),
            ("string", ValueType::String),
            ("token", ValueType::Token),
            ("reference", ValueType::Reference),
            ("quantity", ValueType::Quantity),
            ("uri", ValueType::Uri),
        ] {
            let mut value = body();
            value["type"] = json!(code);
            let spec = ParameterSpec::parse(&value).unwrap_or_else(|_| panic!("{code}"));
            assert_eq!(spec.def.value_type, wanted, "{code}");
        }
        let mut composite = body();
        composite["type"] = json!("composite");
        assert!(matches!(
            ParameterSpec::parse(&composite).unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        let mut special = body();
        special["type"] = json!("special");
        assert!(matches!(
            ParameterSpec::parse(&special).unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
    }

    #[test]
    fn a_status_the_definition_is_no_longer_active_under_marks_it_retired() {
        for status in ["retired", "unknown"] {
            let mut value = body();
            value["status"] = json!(status);
            assert!(ParameterSpec::parse(&value).unwrap().retired, "{status}");
        }
        for status in ["active", "draft"] {
            let mut value = body();
            value["status"] = json!(status);
            assert!(!ParameterSpec::parse(&value).unwrap().retired, "{status}");
        }
    }

    #[test]
    fn an_expression_naming_several_paths_yields_one_path_for_each() {
        let mut value = body();
        value["type"] = json!("reference");
        value["expression"] = json!("Patient.extension.valueReference | Patient.link.other");
        value["target"] = json!(["Organization", "Practitioner"]);
        let spec = ParameterSpec::parse(&value).unwrap();
        assert_eq!(
            spec.def.paths(),
            vec![
                "extension.valueReference".to_owned(),
                "link.other".to_owned()
            ]
        );
        assert_eq!(
            spec.def.targets,
            vec!["Organization".to_owned(), "Practitioner".to_owned()]
        );
    }

    #[test]
    fn a_path_of_a_type_the_definition_does_not_name_is_kept_as_it_stands() {
        let mut value = body();
        value["base"] = json!(["Patient", "Practitioner"]);
        value["expression"] =
            json!("Patient.extension.valueCode | Practitioner.extension.valueCode");
        let spec = ParameterSpec::parse(&value).unwrap();
        assert_eq!(
            spec.def.paths(),
            vec![
                "extension.valueCode".to_owned(),
                "extension.valueCode".to_owned()
            ]
        );
        assert_eq!(spec.base.len(), 2);
    }

    #[test]
    fn a_code_the_server_reserves_for_itself_is_refused() {
        for code in [
            "_id",
            "_lastUpdated",
            "_profile",
            "_tag",
            "_security",
            "_nonesuch",
        ] {
            let mut value = body();
            value["code"] = json!(code);
            assert!(
                matches!(
                    ParameterSpec::parse(&value).unwrap_err(),
                    Error::InvalidParameter(_)
                ),
                "{code}"
            );
        }
    }

    #[test]
    fn an_expression_the_server_cannot_evaluate_as_a_path_is_refused() {
        for expression in [
            "Patient.name.where(use='official')",
            "Patient.deceased.exists()",
            "Patient.name.given.first()",
        ] {
            let mut value = body();
            value["expression"] = json!(expression);
            assert!(
                matches!(
                    ParameterSpec::parse(&value).unwrap_err(),
                    Error::UnsupportedParameter(_)
                ),
                "{expression}"
            );
        }
    }

    #[test]
    fn a_definition_missing_a_member_it_needs_is_refused() {
        for missing in ["url", "code", "type", "expression", "base"] {
            let mut value = body();
            value.as_object_mut().unwrap().remove(missing);
            assert!(ParameterSpec::parse(&value).is_err(), "{missing}");
            let mut empty = body();
            empty[missing] = json!("");
            assert!(ParameterSpec::parse(&empty).is_err(), "{missing} empty");
        }
        let mut kind = body();
        kind["base"] = json!(["Nonesuch"]);
        assert!(ParameterSpec::parse(&kind).is_err());
        let mut target = body();
        target["target"] = json!(["Nonesuch"]);
        assert!(ParameterSpec::parse(&target).is_err());
        assert!(ParameterSpec::parse(&json!({"resourceType": "Patient"})).is_err());
        assert!(ParameterSpec::parse(&json!("not a resource")).is_err());
    }
}
