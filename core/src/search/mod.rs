
pub mod chain;
pub mod compartment;
pub mod grant;
pub mod include;
pub mod index;
pub mod modifier;
pub mod parameter;
pub mod path;
pub mod registry;
pub mod value;

pub use chain::{Chain, ChainDirection, Criterion};
pub use compartment::{Compartment, CompartmentDef, Membership};
pub use grant::Grant;
pub use include::{Include, IncludeDirection};
pub use index::IndexKey;
pub use modifier::Modifier;
pub use parameter::ParameterSpec;
pub use path::select;
pub use registry::{
    common, for_type, lookup, references, CompositeDef, ParamDef, ParamStatus, RegisteredParam, Registry,
    SubDef, Target,
};
pub use value::{Comparator, SearchValue, Token, TokenSystem, ValueType};

use crate::{FhirInstant, ResourceId};
use serde_json::Value;

pub fn pointers(element: &Value) -> Vec<String> {
    match element {
        Value::String(text) => vec![text.clone()],
        Value::Array(items) => items.iter().flat_map(pointers).collect(),
        Value::Object(map) => map
            .get("reference")
            .and_then(Value::as_str)
            .map(|text| vec![text.to_owned()])
            .unwrap_or_default(),
        Value::Bool(_) | Value::Number(_) | Value::Null => Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub name: String,
    pub target: Target,
    pub modifier: Modifier,
    pub values: Vec<SearchValue>,
    pub index: Option<String>,
}

impl Filter {
    pub fn new(name: &str, target: Target, values: Vec<SearchValue>) -> Filter {
        Filter {
            name: name.to_owned(),
            target,
            modifier: Modifier::None,
            values,
            index: None,
        }
    }

    pub fn matches_indexed(&self, elements: &[Value]) -> bool {
        match &self.modifier {
            Modifier::Missing => {
                let wanted = matches!(self.values.first(), Some(SearchValue::Missing(true)));
                elements.iter().all(Value::is_null) == wanted
            }
            modifier if modifier.is_exclusive() => !self
                .values
                .iter()
                .any(|value| self.indexed_hit(value, elements)),
            _ => self.values.iter().any(|value| {
                self.indexed_hit(value, elements) != value.is_negated()
            }),
        }
    }

    fn indexed_hit(&self, value: &SearchValue, elements: &[Value]) -> bool {
        elements
            .iter()
            .any(|element| self.modifier.accepts(value, element))
    }

    pub fn matches(&self, id: &ResourceId, last_updated: &FhirInstant, body: &Value) -> bool {
        match &self.modifier {
            Modifier::Missing => {
                let wanted = matches!(self.values.first(), Some(SearchValue::Missing(true)));
                self.absent(body) == wanted
            }
            modifier if modifier.is_exclusive() => !self
                .values
                .iter()
                .any(|value| self.hit(value, id, last_updated, body)),
            _ => self
                .values
                .iter()
                .any(|value| self.accepts(value, id, last_updated, body)),
        }
    }

    pub fn resolved(&self, values: &[SearchValue]) -> Filter {
        Filter {
            name: self.name.clone(),
            target: self.target.clone(),
            modifier: match self.modifier {
                Modifier::In => Modifier::None,
                Modifier::NotIn => Modifier::Not,
                ref other => other.clone(),
            },
            values: values.to_vec(),
            index: self.index.clone(),
        }
    }

    pub fn expanded(&self, values: &[SearchValue]) -> Filter {
        Filter {
            name: self.name.clone(),
            target: self.target.clone(),
            modifier: Modifier::None,
            values: values.to_vec(),
            index: self.index.clone(),
        }
    }

    pub fn code_sets(&self) -> Vec<String> {
        match self.modifier {
            Modifier::In | Modifier::NotIn => self
                .values
                .iter()
                .filter_map(|value| match value {
                    SearchValue::Uri(text) => Some(text.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn absent(&self, body: &Value) -> bool {
        match &self.target {
            Target::Id | Target::LastUpdated => false,
            Target::Path(paths) => paths
                .iter()
                .flat_map(|path| select(body, path))
                .all(Value::is_null),
            Target::Composite(def) => def
                .base
                .iter()
                .flat_map(|path| select(body, path))
                .all(Value::is_null),
        }
    }

    fn accepts(
        &self,
        value: &SearchValue,
        id: &ResourceId,
        last_updated: &FhirInstant,
        body: &Value,
    ) -> bool {
        self.hit(value, id, last_updated, body) != value.is_negated()
    }

    fn hit(
        &self,
        value: &SearchValue,
        id: &ResourceId,
        last_updated: &FhirInstant,
        body: &Value,
    ) -> bool {
        match &self.target {
            Target::Id => self
                .modifier
                .accepts(value, &Value::String(id.as_str().to_owned())),
            Target::LastUpdated => self
                .modifier
                .accepts(value, &Value::String(last_updated.as_str().to_owned())),
            Target::Path(paths) => paths
                .iter()
                .flat_map(|path| select(body, path))
                .any(|element| self.modifier.accepts(value, element)),
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
        }
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
    target: &Target,
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

pub fn code_set(body: &Value) -> Vec<SearchValue> {
    let mut codes = Vec::new();
    for include in items(select(body, "compose.include")) {
        let system = include.get("system").and_then(Value::as_str);
        for concept in items(select(include, "concept")) {
            if let Some(code) = concept.get("code").and_then(Value::as_str) {
                codes.push(coded(system, code));
            }
        }
    }
    for contains in items(select(body, "expansion.contains")) {
        let system = contains.get("system").and_then(Value::as_str);
        if let Some(code) = contains.get("code").and_then(Value::as_str) {
            codes.push(coded(system, code));
        }
    }
    codes
}

fn items(found: Vec<&Value>) -> Vec<&Value> {
    found
        .into_iter()
        .flat_map(|value| match value {
            Value::Array(entries) => entries.iter().collect(),
            other => vec![other],
        })
        .collect()
}

fn coded(system: Option<&str>, code: &str) -> SearchValue {
    SearchValue::Token(Token {
        system: match system {
            Some(text) => TokenSystem::Exact(text.to_owned()),
            None => TokenSystem::Any,
        },
        code: Some(code.to_owned()),
    })
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
        Filter::new(name, target, values)
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
        let target = Target::path(["birthDate"]);
        let ne = vec![SearchValue::parse(ValueType::Date, "ne1980-04-01").unwrap()];
        assert!(!filter("birthdate", target.clone(), ne).matches(&id(), &updated(), &body));
        let other = vec![SearchValue::parse(ValueType::Date, "ne2020").unwrap()];
        assert!(filter("birthdate", target, other).matches(&id(), &updated(), &body));
    }

    #[test]
    fn a_composite_filter_needs_a_composite_value() {
        let def = lookup(Some("Observation".parse().unwrap()), "code-value-quantity").unwrap();
        let wrong = vec![SearchValue::parse(ValueType::Token, "x").unwrap()];
        assert!(!filter(&def.name, def.target.clone(), wrong).matches(&id(), &updated(), &json!({})));
    }

    #[test]
    fn sort_values_project_every_target() {
        let body = json!({"name": [{"family": "Ann"}], "active": true});
        assert_eq!(
            sort_value(&Target::Id, &id(), &updated(), &body),
            SortValue::Text("r-1".to_owned())
        );
        assert_eq!(
            sort_value(&Target::LastUpdated, &id(), &updated(), &body),
            SortValue::Instant(updated().key())
        );
        assert_eq!(
            sort_value(&Target::path(["name.family"]), &id(), &updated(), &body),
            SortValue::Text("Ann".to_owned())
        );
        assert_eq!(
            sort_value(&Target::path(["name"]), &id(), &updated(), &body),
            SortValue::Text("Ann".to_owned())
        );
        assert_eq!(
            sort_value(&Target::path(["missing"]), &id(), &updated(), &body),
            SortValue::Missing
        );
        assert_eq!(
            sort_value(&Target::path(["active"]), &id(), &updated(), &body),
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

    #[test]
    fn a_code_set_yields_every_code_it_defines() {
        let body = serde_json::json!({
            "resourceType": "ValueSet",
            "compose": {"include": [{"system": "urn:s", "concept": [{"code": "a"}, {"code": "b"}]}]},
            "expansion": {"contains": [{"system": "urn:t", "code": "c"}, {"display": "no code"}]}
        });
        let codes = code_set(&body);
        assert_eq!(codes.len(), 3);
        assert!(codes.contains(&SearchValue::Token(Token {
            system: TokenSystem::Exact("urn:t".to_owned()),
            code: Some("c".to_owned())
        })));
        assert!(code_set(&serde_json::json!({})).is_empty());
    }
}
