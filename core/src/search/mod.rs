
pub mod path;
pub mod registry;
pub mod value;

pub use path::select;
pub use registry::{common, lookup, ParamDef, Target};
pub use value::{Comparator, SearchValue, Token, TokenSystem, ValueType};

use crate::{FhirInstant, ResourceId};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
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
