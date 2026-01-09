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
        .map(|name| name.parse::<ResourceType>().map(|kind| kind.as_str().to_owned()))
        .collect()
}

fn expression(raw: &str, base: &[ResourceType]) -> Result<Vec<String>, Error> {
    let mut paths = Vec::new();
    for part in raw.split('|').map(str::trim).filter(|part| !part.is_empty()) {
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
    fn a_definition_becomes_a_parameter() {
        let spec = ParameterSpec::parse(&body()).unwrap();
        assert_eq!(spec.url, "urn:p:risk-band");
        assert_eq!(spec.code(), "risk-band");
        assert_eq!(spec.def.value_type, ValueType::Token);
        assert_eq!(spec.def.paths(), vec!["extension.valueCode".to_owned()]);
        assert_eq!(spec.base, vec!["Patient".parse::<ResourceType>().unwrap()]);
        assert!(!spec.retired);
        assert!(!spec.def.sortable);
    }

    #[test]
    fn a_withdrawn_definition_is_marked_retired() {
        let mut value = body();
        value["status"] = json!("retired");
        assert!(ParameterSpec::parse(&value).unwrap().retired);
    }

    #[test]
    fn several_paths_and_targets_are_read() {
        let mut value = body();
        value["type"] = json!("reference");
        value["expression"] = json!("Patient.extension.valueReference | Patient.link.other");
        value["target"] = json!(["Organization", "Practitioner"]);
        let spec = ParameterSpec::parse(&value).unwrap();
        assert_eq!(spec.def.paths().len(), 2);
        assert_eq!(spec.def.targets, vec!["Organization".to_owned(), "Practitioner".to_owned()]);
    }

    #[test]
    fn a_malformed_definition_is_rejected() {
        for missing in ["url", "code", "type", "expression", "base"] {
            let mut value = body();
            value.as_object_mut().unwrap().remove(missing);
            assert!(ParameterSpec::parse(&value).is_err(), "{missing}");
        }
        let mut reserved = body();
        reserved["code"] = json!("_id");
        assert!(matches!(
            ParameterSpec::parse(&reserved).unwrap_err(),
            Error::InvalidParameter(_)
        ));
        let mut kind = body();
        kind["type"] = json!("composite");
        assert!(matches!(
            ParameterSpec::parse(&kind).unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        let mut expression = body();
        expression["expression"] = json!("Patient.name.where(use='official')");
        assert!(matches!(
            ParameterSpec::parse(&expression).unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        let mut kind = body();
        kind["base"] = json!(["Nonesuch"]);
        assert!(ParameterSpec::parse(&kind).is_err());
        let mut target = body();
        target["target"] = json!(["Nonesuch"]);
        assert!(ParameterSpec::parse(&target).is_err());
        assert!(ParameterSpec::parse(&json!({"resourceType": "Patient"})).is_err());
    }
}
