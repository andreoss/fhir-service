use crate::search::value::{SearchValue, ValueType};
use crate::{Error, FhirVersion, ResourceType};
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

    pub fn applies_in(&self, value_type: ValueType, version: FhirVersion) -> bool {
        match self {
            Modifier::Text if value_type == ValueType::Reference => version == FhirVersion::R5,
            other => other.applies_to(value_type),
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
            Modifier::Below | Modifier::Above => match value {
                SearchValue::Token(_) => value.matches(element),
                _ => hierarchy(value, element, matches!(self, Modifier::Below)),
            },
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
    let Some(wanted) = wanted(value) else {
        return false;
    };
    let mut found = Vec::new();
    plain(element, &mut found);
    found.iter().any(|text| text == wanted)
}

fn contains(value: &SearchValue, element: &Value) -> bool {
    let Some(wanted) = wanted(value) else {
        return false;
    };
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
    let Some(wanted) = wanted(value) else {
        return false;
    };
    let wanted = wanted.to_lowercase();
    let mut found = Vec::new();
    texts(element, &mut found);
    found
        .iter()
        .any(|text| text.to_lowercase().contains(&wanted))
}

fn hierarchy(value: &SearchValue, element: &Value, below: bool) -> bool {
    let Some(wanted) = wanted(value) else {
        return false;
    };
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
    if child.starts_with(URN) || ancestor.starts_with(URN) {
        return false;
    }
    child
        .strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('/'))
}

const URN: &str = "urn:";

fn typed_reference(value: &SearchValue, resource_type: &ResourceType, element: &Value) -> bool {
    let Some(wanted) = wanted(value) else {
        return false;
    };
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
        Value::Object(map) => map
            .get("identifier")
            .is_some_and(|found| value.matches(found)),
        _ => false,
    }
}

pub fn value_of(modifier: &Modifier, declared: ValueType, raw: &str) -> Result<SearchValue, Error> {
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
        Modifier::Type(resource_type) => {
            if let Some((head, _)) = raw.rsplit_once('/') {
                let named = head.rsplit('/').next().unwrap_or(head);
                if named != resource_type.as_str() {
                    return Err(Error::UnsupportedParameter(format!(
                        "modifier :{} contradicts {raw:?}",
                        resource_type.as_str()
                    )));
                }
            }
            SearchValue::parse(declared, raw)
        }
        other => SearchValue::parse(other.value_type(declared), raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_modifier_the_value_set_publishes_is_read_from_its_spelling() {
        for (spelling, wanted) in [
            ("missing", Modifier::Missing),
            ("exact", Modifier::Exact),
            ("contains", Modifier::Contains),
            ("not", Modifier::Not),
            ("text", Modifier::Text),
            ("in", Modifier::In),
            ("not-in", Modifier::NotIn),
            ("below", Modifier::Below),
            ("above", Modifier::Above),
            ("identifier", Modifier::Identifier),
            ("of-type", Modifier::OfType),
        ] {
            assert_eq!(spelling.parse::<Modifier>().unwrap(), wanted, "{spelling}");
        }
        assert_eq!(
            "Patient".parse::<Modifier>().unwrap(),
            Modifier::Type("Patient".parse().unwrap())
        );
        assert!("nonesuch".parse::<Modifier>().is_err());
        assert!("Below".parse::<Modifier>().is_err());
    }

    #[test]
    fn a_modifier_applies_to_the_types_the_specification_gives_it() {
        let published: &[(Modifier, &[ValueType])] = &[
            (
                Modifier::Missing,
                &[
                    ValueType::Number,
                    ValueType::Date,
                    ValueType::String,
                    ValueType::Token,
                    ValueType::Quantity,
                    ValueType::Reference,
                    ValueType::Composite,
                    ValueType::Uri,
                ],
            ),
            (Modifier::Exact, &[ValueType::String]),
            (Modifier::Contains, &[ValueType::String]),
            (Modifier::Not, &[ValueType::Token]),
            (Modifier::Text, &[ValueType::Token]),
            (Modifier::In, &[ValueType::Token]),
            (Modifier::NotIn, &[ValueType::Token]),
            (Modifier::OfType, &[ValueType::Token]),
            (
                Modifier::Below,
                &[ValueType::Token, ValueType::Uri, ValueType::Reference],
            ),
            (
                Modifier::Above,
                &[ValueType::Token, ValueType::Uri, ValueType::Reference],
            ),
            (Modifier::Identifier, &[ValueType::Reference]),
        ];
        let every = [
            ValueType::Number,
            ValueType::Date,
            ValueType::String,
            ValueType::Token,
            ValueType::Quantity,
            ValueType::Reference,
            ValueType::Composite,
            ValueType::Uri,
        ];
        for (modifier, allowed) in published {
            for value_type in every {
                assert_eq!(
                    modifier.applies_to(value_type),
                    allowed.contains(&value_type),
                    "{modifier:?} on {value_type:?}"
                );
            }
        }
    }

    #[test]
    fn a_text_modifier_is_allowed_on_a_reference_only_in_r5() {
        for version in FhirVersion::ALL {
            let allowed = Modifier::Text.applies_in(ValueType::Reference, version);
            assert_eq!(allowed, version == FhirVersion::R5, "{version:?}");
            assert!(
                Modifier::Text.applies_in(ValueType::Token, version),
                "{version:?}"
            );
            assert!(
                !Modifier::Text.applies_in(ValueType::String, version),
                "{version:?}"
            );
        }
    }

    #[test]
    fn a_value_the_modifier_cannot_carry_is_rejected() {
        assert!(matches!(
            value_of(&Modifier::Missing, ValueType::Token, "yes").unwrap_err(),
            Error::InvalidParameter(_)
        ));
        assert!(matches!(
            value_of(&Modifier::Exact, ValueType::Token, "a").unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        assert!(value_of(&Modifier::In, ValueType::Token, "http://x").is_ok());
        assert_eq!(
            value_of(&Modifier::In, ValueType::Token, "http://x").unwrap(),
            SearchValue::Uri("http://x".to_owned())
        );
        assert_eq!(
            value_of(&Modifier::Text, ValueType::Token, "fever").unwrap(),
            SearchValue::Text("fever".to_owned())
        );
        assert_eq!(
            value_of(&Modifier::Identifier, ValueType::Reference, "urn:mrn|42").unwrap(),
            SearchValue::parse(ValueType::Token, "urn:mrn|42").unwrap()
        );
    }

    #[test]
    fn a_reference_that_already_names_a_type_refuses_a_type_modifier_naming_another() {
        let patient = Modifier::Type("Patient".parse().unwrap());
        assert!(matches!(
            value_of(&patient, ValueType::Reference, "Group/g-1").unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        assert!(matches!(
            value_of(&patient, ValueType::Reference, "http://x/fhir/Group/g-1").unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        assert_eq!(
            value_of(&patient, ValueType::Reference, "Patient/p-1").unwrap(),
            SearchValue::Reference("Patient/p-1".to_owned())
        );
        assert_eq!(
            value_of(&patient, ValueType::Reference, "p-1").unwrap(),
            SearchValue::Reference("p-1".to_owned())
        );
    }

    #[test]
    fn a_type_modifier_admits_only_a_reference_of_that_type() {
        let patient = Modifier::Type("Patient".parse().unwrap());
        let group = Modifier::Type("Group".parse().unwrap());
        let value = SearchValue::Reference("p-9".to_owned());
        let element = json!({"reference": "Patient/p-9"});
        assert!(patient.accepts(&value, &element));
        assert!(!group.accepts(&value, &element));
        assert!(!patient.accepts(&value, &json!({"reference": "Patient/p-8"})));
    }

    #[test]
    fn a_subsumption_modifier_never_reads_a_hierarchy_into_the_spelling_of_a_code() {
        let below = Modifier::Below;
        let above = Modifier::Above;
        let code = SearchValue::parse(ValueType::Token, "a.b").unwrap();
        let element = json!({"coding": [{"code": "a.b.c"}]});
        assert!(
            !below.accepts(&code, &element),
            "a code is not a dotted path"
        );
        assert!(!above.accepts(
            &SearchValue::parse(ValueType::Token, "a.b.c.d").unwrap(),
            &element
        ));
        let itself = SearchValue::parse(ValueType::Token, "a.b.c").unwrap();
        assert!(below.accepts(&itself, &element));
        assert!(above.accepts(&itself, &element));
        let qualified = SearchValue::parse(ValueType::Token, "urn:other|a.b.c").unwrap();
        assert!(!below.accepts(
            &qualified,
            &json!({"coding": [{"system": "urn:s", "code": "a.b.c"}]})
        ));
    }

    #[test]
    fn a_subsumption_modifier_on_a_uri_walks_the_address_it_is_a_prefix_of() {
        let below = Modifier::Below;
        let above = Modifier::Above;
        let stored = json!("http://x/base/part");
        let base = SearchValue::parse(ValueType::Uri, "http://x/base").unwrap();
        assert!(below.accepts(&base, &stored));
        assert!(!above.accepts(&base, &stored));
        let deeper = SearchValue::parse(ValueType::Uri, "http://x/base/part/deep").unwrap();
        assert!(above.accepts(&deeper, &stored));
        assert!(!below.accepts(&deeper, &stored));
        let sibling = SearchValue::parse(ValueType::Uri, "http://x/bas").unwrap();
        assert!(!below.accepts(&sibling, &stored));
        let urn = json!("urn:oid:1.2.3.4");
        let stem = SearchValue::parse(ValueType::Uri, "urn:oid:1.2").unwrap();
        assert!(!below.accepts(&stem, &urn), "a urn carries no hierarchy");
        let same = SearchValue::parse(ValueType::Uri, "urn:oid:1.2.3.4").unwrap();
        assert!(below.accepts(&same, &urn));
    }

    #[test]
    fn a_set_membership_modifier_is_answered_by_an_expansion_and_never_by_the_address() {
        let address = value_of(&Modifier::In, ValueType::Token, "http://x/vs").unwrap();
        let coded = json!({"coding": [{"system": "urn:s", "code": "a"}]});
        assert!(!Modifier::In.accepts(&address, &coded));
        assert!(!Modifier::NotIn.accepts(&address, &coded));
        assert!(!Modifier::In.is_exclusive());
        assert!(Modifier::NotIn.is_exclusive());
    }

    #[test]
    fn an_unusable_value_never_accepts_an_element() {
        let composite = SearchValue::composite(
            SearchValue::parse(ValueType::Token, "a").unwrap(),
            SearchValue::parse(ValueType::Token, "b").unwrap(),
        );
        let element = json!("a");
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
