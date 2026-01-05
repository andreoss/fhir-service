use crate::search::value::{SearchValue, ValueType};
use crate::{Error, ResourceType};
use serde_json::Value;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modifier {
    None,
    Missing,
    Exact,
    Contains,
    Not,
    Text,
    In,
    NotIn,
    Below,
    Above,
    Type(ResourceType),
    Identifier,
    OfType,
}

impl FromStr for Modifier {
    type Err = Error;

    fn from_str(text: &str) -> Result<Modifier, Error> {
        match text {
            "missing" => Ok(Modifier::Missing),
            "exact" => Ok(Modifier::Exact),
            "contains" => Ok(Modifier::Contains),
            "not" => Ok(Modifier::Not),
            "text" => Ok(Modifier::Text),
            "in" => Ok(Modifier::In),
            "not-in" => Ok(Modifier::NotIn),
            "below" => Ok(Modifier::Below),
            "above" => Ok(Modifier::Above),
            "identifier" => Ok(Modifier::Identifier),
            "of-type" => Ok(Modifier::OfType),
            other => ResourceType::from_str(other)
                .map(Modifier::Type)
                .map_err(|_| Error::UnsupportedParameter(format!("modifier {other:?}"))),
        }
    }
}

impl Modifier {
    pub fn applies_to(&self, value_type: ValueType) -> bool {
        match self {
            Modifier::None | Modifier::Missing => true,
            Modifier::Exact | Modifier::Contains => value_type == ValueType::String,
            Modifier::Not | Modifier::Text | Modifier::In | Modifier::NotIn | Modifier::OfType => {
                value_type == ValueType::Token
            }
            Modifier::Below | Modifier::Above => matches!(
                value_type,
                ValueType::Token | ValueType::Uri | ValueType::Reference
            ),
            Modifier::Type(_) | Modifier::Identifier => value_type == ValueType::Reference,
        }
    }

    pub fn value_type(&self, declared: ValueType) -> ValueType {
        match self {
            Modifier::Exact | Modifier::Contains | Modifier::Text => ValueType::String,
            Modifier::In | Modifier::NotIn => ValueType::Uri,
            Modifier::Identifier => ValueType::Token,
            _ => declared,
        }
    }

    pub fn is_exclusive(&self) -> bool {
        matches!(self, Modifier::Not | Modifier::NotIn)
    }

    pub fn accepts(&self, value: &SearchValue, element: &Value) -> bool {
        match self {
            Modifier::None | Modifier::Not | Modifier::In | Modifier::NotIn => {
                value.matches(element)
            }
            Modifier::Missing => false,
            Modifier::Exact => exact(value, element),
            Modifier::Contains => contains(value, element),
            Modifier::Text => narrative(value, element),
            Modifier::Below => hierarchy(value, element, true),
            Modifier::Above => hierarchy(value, element, false),
            Modifier::Type(resource_type) => typed_reference(value, resource_type, element),
            Modifier::Identifier => identifier(value, element),
            Modifier::OfType => value.matches(element),
        }
    }
}

fn wanted(value: &SearchValue) -> Option<&str> {
    match value {
        SearchValue::Text(text) | SearchValue::Uri(text) | SearchValue::Reference(text) => {
            Some(text)
        }
        SearchValue::Token(token) => token.code.as_deref(),
        _ => None,
    }
}

fn strings(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
        Value::Object(map) => {
            for name in ["coding", "code", "value", "reference", "url"] {
                if let Some(found) = map.get(name) {
                    strings(found, out);
                }
            }
        }
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn texts(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::Array(items) => items.iter().for_each(|item| texts(item, out)),
        Value::Object(map) => {
            for name in ["text", "display"] {
                if let Some(Value::String(found)) = map.get(name) {
                    out.push(found.clone());
                }
            }
            if let Some(found) = map.get("coding") {
                texts(found, out);
            }
        }
        Value::String(text) => out.push(text.clone()),
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn exact(value: &SearchValue, element: &Value) -> bool {
    let Some(wanted) = wanted(value) else { return false };
    let mut found = Vec::new();
    plain(element, &mut found);
    found.iter().any(|text| text == wanted)
}

fn contains(value: &SearchValue, element: &Value) -> bool {
    let Some(wanted) = wanted(value) else { return false };
    let wanted = wanted.to_lowercase();
    let mut found = Vec::new();
    plain(element, &mut found);
    found
        .iter()
        .any(|text| text.to_lowercase().contains(&wanted))
}

fn plain(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| plain(item, out)),
        Value::Object(map) => map.values().for_each(|nested| plain(nested, out)),
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn narrative(value: &SearchValue, element: &Value) -> bool {
    let Some(wanted) = wanted(value) else { return false };
    let wanted = wanted.to_lowercase();
    let mut found = Vec::new();
    texts(element, &mut found);
    found
        .iter()
        .any(|text| text.to_lowercase().contains(&wanted))
}

fn hierarchy(value: &SearchValue, element: &Value, below: bool) -> bool {
    let Some(wanted) = wanted(value) else { return false };
    let mut found = Vec::new();
    strings(element, &mut found);
    found.iter().any(|stored| {
        if below {
            descends(stored, wanted)
        } else {
            descends(wanted, stored)
        }
    })
}

fn descends(child: &str, ancestor: &str) -> bool {
    if child == ancestor {
        return true;
    }
    child
        .strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('/'))
}

fn typed_reference(value: &SearchValue, resource_type: &ResourceType, element: &Value) -> bool {
    let Some(wanted) = wanted(value) else { return false };
    let mut found = Vec::new();
    strings(element, &mut found);
    found.iter().any(|stored| {
        let mut parts = stored.rsplit('/');
        let id = parts.next().unwrap_or_default();
        let stored_type = parts.next().unwrap_or_default();
        stored_type == resource_type.as_str()
            && (id == wanted || *stored == wanted || format!("{stored_type}/{id}") == wanted)
    })
}

fn identifier(value: &SearchValue, element: &Value) -> bool {
    match element {
        Value::Array(items) => items.iter().any(|item| identifier(value, item)),
        Value::Object(map) => map.get("identifier").is_some_and(|found| value.matches(found)),
        _ => false,
    }
}

pub fn value_of(
    modifier: &Modifier,
    declared: ValueType,
    raw: &str,
) -> Result<SearchValue, Error> {
    if !modifier.applies_to(declared) {
        return Err(Error::UnsupportedParameter(format!(
            "modifier on {raw:?} does not apply to this parameter"
        )));
    }
    match modifier {
        Modifier::Missing => match raw {
            "true" => Ok(SearchValue::Missing(true)),
            "false" => Ok(SearchValue::Missing(false)),
            other => Err(Error::InvalidParameter(format!(":missing {other:?}"))),
        },
        Modifier::OfType => SearchValue::of_type(raw),
        other => SearchValue::parse(other.value_type(declared), raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modifier_is_read_from_its_spelling() {
        assert_eq!("missing".parse::<Modifier>().unwrap(), Modifier::Missing);
        assert_eq!("not-in".parse::<Modifier>().unwrap(), Modifier::NotIn);
        assert_eq!(
            "Patient".parse::<Modifier>().unwrap(),
            Modifier::Type("Patient".parse().unwrap())
        );
        assert!("nonesuch".parse::<Modifier>().is_err());
    }

    #[test]
    fn a_value_the_modifier_cannot_carry_is_rejected() {
        assert!(value_of(&Modifier::Missing, ValueType::Token, "yes").is_err());
        assert!(value_of(&Modifier::Exact, ValueType::Token, "a").is_err());
        assert!(value_of(&Modifier::In, ValueType::Token, "http://x").is_ok());
    }

    #[test]
    fn an_unusable_value_never_accepts_an_element() {
        let composite = SearchValue::composite(
            SearchValue::parse(ValueType::Token, "a").unwrap(),
            SearchValue::parse(ValueType::Token, "b").unwrap(),
        );
        let element = serde_json::json!("a");
        for modifier in [
            Modifier::Exact,
            Modifier::Contains,
            Modifier::Text,
            Modifier::Below,
            Modifier::Above,
            Modifier::Identifier,
            Modifier::Missing,
        ] {
            assert!(!modifier.accepts(&composite, &element), "{modifier:?}");
        }
        let typed = Modifier::Type("Patient".parse().unwrap());
        assert!(!typed.accepts(&composite, &element));
    }
}
