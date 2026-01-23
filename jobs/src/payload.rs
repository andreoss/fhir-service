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

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Value {
        serde_json::from_str(text).expect("the fixture is json")
    }

    #[test]
    fn a_count_is_read_from_a_number_or_from_text() {
        let held = parsed(r#"{"a":3,"b":"4","c":null}"#);
        assert_eq!(count(&held, "a").unwrap(), Some(3));
        assert_eq!(count(&held, "b").unwrap(), Some(4));
        assert_eq!(count(&held, "c").unwrap(), None);
        assert_eq!(count(&held, "missing").unwrap(), None);
    }

    #[test]
    fn a_count_that_is_not_a_count_is_refused() {
        let held = parsed(r#"{"a":"many","b":-2,"c":[1]}"#);
        assert!(count(&held, "a").is_err());
        assert!(count(&held, "b").is_err());
        assert!(count(&held, "c").is_err());
    }

    #[test]
    fn a_flag_holds_whether_it_arrived_as_text_or_as_a_value() {
        let held = parsed(r#"{"a":true,"b":"TRUE","c":"no","d":1}"#);
        assert!(flag(&held, "a"));
        assert!(flag(&held, "b"));
        assert!(!flag(&held, "c"));
        assert!(!flag(&held, "d"));
        assert!(flagged(&held, &["c", "a"]));
        assert!(!flagged(&held, &["c", "missing"]));
    }

    #[test]
    fn the_first_spelling_that_carries_names_wins() {
        let held = parsed(r#"{"_type":["Patient"],"types":["Observation"],"text":"a, b"}"#);
        assert_eq!(named(&held, &["_type", "types"]), vec!["Patient"]);
        assert_eq!(named(&held, &["types"]), vec!["Observation"]);
        assert!(named(&held, &["nothing"]).is_empty());
        assert_eq!(listed(&held, "text"), vec!["a", "b"]);
        assert_eq!(text(&held, "text").as_deref(), Some("a, b"));
        assert_eq!(text(&held, "_type"), None);
    }

    #[test]
    fn a_body_that_is_not_json_is_refused() {
        assert!(body("{ not json").is_err());
        assert!(body("{}").is_ok());
    }
}
