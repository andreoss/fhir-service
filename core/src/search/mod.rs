
pub mod path;
pub mod registry;
pub mod value;

pub use path::select;
pub use registry::{common, lookup, CompositeDef, ParamDef, SubDef, Target};
pub use value::{Comparator, SearchValue, Token, TokenSystem, ValueType};

use crate::{FhirInstant, ResourceId};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub name: String,
    pub target: Target,
    pub values: Vec<SearchValue>,
}

impl Filter {
    pub fn matches(&self, id: &ResourceId, last_updated: &FhirInstant, body: &Value) -> bool {
        self.values
            .iter()
            .any(|value| self.accepts(value, id, last_updated, body))
    }

    fn accepts(
        &self,
        value: &SearchValue,
        id: &ResourceId,
        last_updated: &FhirInstant,
        body: &Value,
    ) -> bool {
        let hit = match self.target {
            Target::Id => value.matches(&Value::String(id.as_str().to_owned())),
            Target::LastUpdated => value.matches(&Value::String(last_updated.as_str().to_owned())),
            Target::Path(paths) => paths
                .iter()
                .flat_map(|path| select(body, path))
                .any(|element| value.matches(element)),
            Target::Composite(def) => match value.components() {
                Some((left, right)) => def
                    .base
                    .iter()
                    .flat_map(|path| select(body, path))
                    .any(|element| {
                        component(&def.left, left, element)
                            && component(&def.right, right, element)
                    }),
                None => false,
            },
        };
        hit != value.is_negated()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortValue {
    Instant(crate::InstantKey),
    Text(String),
    Missing,
}

impl PartialOrd for SortValue {
    fn partial_cmp(&self, other: &SortValue) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SortValue {
    fn cmp(&self, other: &SortValue) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (SortValue::Instant(left), SortValue::Instant(right)) => left.cmp(right),
            (SortValue::Text(left), SortValue::Text(right)) => {
                left.to_lowercase().cmp(&right.to_lowercase())
            }
            (SortValue::Missing, SortValue::Missing) => Ordering::Equal,
            (SortValue::Missing, _) => Ordering::Greater,
            (_, SortValue::Missing) => Ordering::Less,
            (SortValue::Instant(_), SortValue::Text(_)) => Ordering::Less,
            (SortValue::Text(_), SortValue::Instant(_)) => Ordering::Greater,
        }
    }
}

pub fn sort_value(
    target: Target,
    id: &ResourceId,
    last_updated: &FhirInstant,
    body: &Value,
) -> SortValue {
    match target {
        Target::Id => SortValue::Text(id.as_str().to_owned()),
        Target::LastUpdated => SortValue::Instant(last_updated.key()),
        Target::Path(paths) => paths
            .iter()
            .flat_map(|path| select(body, path))
            .find_map(scalar)
            .map(SortValue::Text)
            .unwrap_or(SortValue::Missing),
        Target::Composite(_) => SortValue::Missing,
    }
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Object(map) => map
            .values()
            .find_map(|nested| matches!(nested, Value::String(_)).then(|| scalar(nested))?),
        Value::Array(items) => items.iter().find_map(scalar),
        Value::Null => None,
    }
}

fn component(sub: &SubDef, value: &SearchValue, element: &Value) -> bool {
    sub.paths
        .iter()
        .flat_map(|path| select(element, path))
        .any(|found| value.matches(found))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::value::ValueType;
    use serde_json::json;

    fn id() -> ResourceId {
        ResourceId::parse("r-1").unwrap()
    }

    fn updated() -> FhirInstant {
        FhirInstant::parse("2026-09-06T04:00:00Z").unwrap()
    }

    fn filter(name: &str, target: Target, values: Vec<SearchValue>) -> Filter {
        Filter {
            name: name.to_owned(),
            target,
            values,
        }
    }

    #[test]
    fn a_filter_accepts_any_of_its_values() {
        let values = vec![
            SearchValue::parse(ValueType::Token, "r-2").unwrap(),
            SearchValue::parse(ValueType::Token, "r-1").unwrap(),
        ];
        assert!(filter("_id", Target::Id, values).matches(&id(), &updated(), &json!({})));
    }

    #[test]
    fn a_negated_value_needs_every_element_to_fail() {
        let body = json!({"birthDate": ["1980-04-01", "1995-11-20"]});
        let target = Target::Path(&["birthDate"]);
        let ne = vec![SearchValue::parse(ValueType::Date, "ne1980-04-01").unwrap()];
        assert!(!filter("birthdate", target, ne).matches(&id(), &updated(), &body));
        let other = vec![SearchValue::parse(ValueType::Date, "ne2020").unwrap()];
        assert!(filter("birthdate", target, other).matches(&id(), &updated(), &body));
    }

    #[test]
    fn a_composite_filter_needs_a_composite_value() {
        let def = lookup(Some("Observation".parse().unwrap()), "code-value-quantity").unwrap();
        let wrong = vec![SearchValue::parse(ValueType::Token, "x").unwrap()];
        assert!(!filter(def.name, def.target, wrong).matches(&id(), &updated(), &json!({})));
    }

    #[test]
    fn sort_values_project_every_target() {
        let body = json!({"name": [{"family": "Ann"}], "active": true});
        assert_eq!(
            sort_value(Target::Id, &id(), &updated(), &body),
            SortValue::Text("r-1".to_owned())
        );
        assert_eq!(
            sort_value(Target::LastUpdated, &id(), &updated(), &body),
            SortValue::Instant(updated().key())
        );
        assert_eq!(
            sort_value(Target::Path(&["name.family"]), &id(), &updated(), &body),
            SortValue::Text("Ann".to_owned())
        );
        assert_eq!(
            sort_value(Target::Path(&["name"]), &id(), &updated(), &body),
            SortValue::Text("Ann".to_owned())
        );
        assert_eq!(
            sort_value(Target::Path(&["missing"]), &id(), &updated(), &body),
            SortValue::Missing
        );
        assert_eq!(
            sort_value(Target::Path(&["active"]), &id(), &updated(), &body),
            SortValue::Text("true".to_owned())
        );
    }

    #[test]
    fn a_missing_sort_value_orders_last_in_either_direction() {
        let text = SortValue::Text("a".to_owned());
        let instant = SortValue::Instant(updated().key());
        assert!(SortValue::Missing > text);
        assert!(text < SortValue::Missing);
        assert_eq!(SortValue::Missing.cmp(&SortValue::Missing), std::cmp::Ordering::Equal);
        assert!(instant < text);
        assert!(text > instant);
        assert_eq!(text.partial_cmp(&SortValue::Text("B".to_owned())), Some(std::cmp::Ordering::Less));
    }
}
