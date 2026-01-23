use fhir_core::{Error, ResourceType};
use serde_json::Value;

pub fn body(payload: &str) -> Result<Value, Error> {
    serde_json::from_str(payload).map_err(|error| Error::InvalidJson(error.to_string()))
}

pub fn text(payload: &Value, name: &str) -> Option<String> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|found| !found.trim().is_empty())
}

pub fn listed(payload: &Value, name: &str) -> Vec<String> {
    match payload.get(name) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(text)) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

pub fn named(payload: &Value, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|name| listed(payload, name))
        .find(|found| !found.is_empty())
        .unwrap_or_default()
}

pub fn flag(payload: &Value, name: &str) -> bool {
    match payload.get(name) {
        Some(Value::Bool(held)) => *held,
        Some(Value::String(held)) => held.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

pub fn count(payload: &Value, name: &str) -> Result<Option<u64>, Error> {
    match payload.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(held)) => held
            .as_u64()
            .map(Some)
            .ok_or_else(|| Error::InvalidParameter(format!("{name} {held} is not a count"))),
        Some(Value::String(held)) => held
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| Error::InvalidParameter(format!("{name} {held:?} is not a count"))),
        Some(other) => Err(Error::InvalidParameter(format!(
            "{name} {other} is not a count"
        ))),
    }
}

pub fn resource_types(names: &[String]) -> Result<Vec<ResourceType>, Error> {
    names
        .iter()
        .map(|name| name.parse::<ResourceType>())
        .collect()
}

pub fn flagged(payload: &Value, names: &[&str]) -> bool {
    names.iter().any(|name| flag(payload, name))
}
