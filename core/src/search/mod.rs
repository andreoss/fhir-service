pub mod chain;
pub mod compartment;
pub mod errata;
pub mod grant;
pub mod include;
pub mod index;
pub mod membership;
pub mod modifier;
pub mod parameter;
pub mod path;
pub mod published;
pub mod registry;
pub mod text;
pub mod value;

pub use chain::{Chain, ChainDirection, Criterion};
pub use compartment::{Compartment, CompartmentDef, Membership};
pub use grant::{Grant, GrantFilter};
pub use include::{Include, IncludeDirection};
pub use index::IndexKey;
pub use membership::{active, collection_types, identity, is_collection, COLLECTIONS};
pub use modifier::Modifier;
pub use parameter::ParameterSpec;
pub use path::select;
pub use registry::{
    common, common_in, for_type, for_type_in, lookup, lookup_in, references, references_in,
    CompositeDef, ParamDef, ParamStatus, RegisteredParam, Registry, SubDef, Target,
};
pub use value::{Comparator, SearchValue, Token, TokenSystem, ValueType};

use crate::{FhirInstant, FhirVersion, ResourceId, ResourceType};
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

    pub exempt: Vec<ResourceType>,
}

impl Filter {
    pub fn new(name: &str, target: Target, values: Vec<SearchValue>) -> Filter {
        Filter {
            name: name.to_owned(),
            target,
            modifier: Modifier::None,
            values,
            index: None,
            exempt: Vec::new(),
        }
    }

    pub fn exempting(mut self, types: Vec<ResourceType>) -> Filter {
        self.exempt = types;
        self
    }

    pub fn exempts(&self, body: &Value) -> bool {
        if self.exempt.is_empty() {
            return false;
        }
        body.get("resourceType")
            .and_then(Value::as_str)
            .is_some_and(|held| self.exempt.iter().any(|kind| kind.as_str() == held))
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
            _ => self
                .values
                .iter()
                .any(|value| self.indexed_hit(value, elements) != value.is_negated()),
        }
    }

    fn indexed_hit(&self, value: &SearchValue, elements: &[Value]) -> bool {
        if self.name == "_text" && self.modifier == Modifier::None {
            let document = elements
                .iter()
                .map(text::visible)
                .collect::<Vec<String>>()
                .join(" ");
            return match value {
                SearchValue::Text(raw) => {
                    text::text_query(raw).is_ok_and(|query| query.matches(&document))
                }
                _ => false,
            };
        }
        elements
            .iter()
            .any(|element| self.modifier.accepts(value, element))
    }

    pub fn matches(&self, id: &ResourceId, last_updated: &FhirInstant, body: &Value) -> bool {
        if self.exempts(body) {
            return true;
        }
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
            exempt: self.exempt.clone(),
        }
    }

    pub fn expanded(&self, values: &[SearchValue]) -> Filter {
        Filter {
            name: self.name.clone(),
            target: self.target.clone(),
            modifier: Modifier::None,
            values: values.to_vec(),
            index: self.index.clone(),
            exempt: self.exempt.clone(),
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
            Target::Id | Target::LastUpdated | Target::Collection => false,
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
            Target::Path(paths) => {
                if self.name == "_text" && self.modifier == Modifier::None {
                    let document = paths
                        .iter()
                        .flat_map(|path| select(body, path))
                        .map(text::visible)
                        .collect::<Vec<String>>()
                        .join(" ");
                    return match value {
                        SearchValue::Text(raw) => {
                            text::text_query(raw).is_ok_and(|query| query.matches(&document))
                        }
                        _ => false,
                    };
                }
                paths
                    .iter()
                    .flat_map(|path| select(body, path))
                    .any(|element| self.modifier.accepts(value, element))
            }
            Target::Collection => false,
            Target::Composite(def) => match value.components() {
                Some((left, right)) => {
                    def.base
                        .iter()
                        .flat_map(|path| select(body, path))
                        .any(|element| {
                            component(&def.left, left, element)
                                && component(&def.right, right, element)
                        })
                }
                None => false,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub enum SortValue {
    Instant(crate::InstantKey),
    Text(String),
    Missing,
}

impl PartialEq for SortValue {
    fn eq(&self, other: &SortValue) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for SortValue {}

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
        Target::Composite(_) | Target::Collection => SortValue::Missing,
    }
}

pub fn code_set(body: &Value) -> Vec<SearchValue> {
    let mut codes = composed(body, "compose.include");
    let excluded = composed(body, "compose.exclude");
    codes.retain(|code| !excluded.contains(code));
    for contains in items(select(body, "expansion.contains")) {
        gather(contains, &mut codes);
    }
    codes
}

fn composed(body: &Value, path: &str) -> Vec<SearchValue> {
    let mut codes = Vec::new();
    for rule in items(select(body, path)) {
        let system = rule.get("system").and_then(Value::as_str);
        for concept in items(select(rule, "concept")) {
            if let Some(code) = concept.get("code").and_then(Value::as_str) {
                codes.push(coded(system, code));
            }
        }
    }
    codes
}

fn gather(contains: &Value, codes: &mut Vec<SearchValue>) {
    let system = contains.get("system").and_then(Value::as_str);
    if let Some(code) = contains.get("code").and_then(Value::as_str) {
        codes.push(coded(system, code));
    }
    for nested in items(select(contains, "contains")) {
        gather(nested, codes);
    }
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

pub fn unsupported(version: FhirVersion) -> &'static [&'static str] {
    match version {
        FhirVersion::Stu3 | FhirVersion::R4 | FhirVersion::R4b | FhirVersion::R5 => {
            &["_content", "_filter", "_query"]
        }
    }
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
        assert!(!filter(&def.name, def.target.clone(), wrong).matches(
            &id(),
            &updated(),
            &json!({})
        ));
    }

    fn indexed(name: &str, target: Target, modifier: Modifier, values: Vec<SearchValue>) -> Filter {
        Filter {
            name: name.to_owned(),
            target,
            modifier,
            values,
            index: Some("urn:p:custom".to_owned()),
            exempt: Vec::new(),
        }
    }

    #[test]
    fn an_indexed_value_answers_as_the_same_value_in_a_body_does() {
        let cases: &[(ValueType, &str, Modifier, Value)] = &[
            (ValueType::Token, "amber", Modifier::None, json!("amber")),
            (ValueType::Token, "amber", Modifier::None, json!("green")),
            (ValueType::Token, "amber", Modifier::Not, json!("amber")),
            (ValueType::Token, "amber", Modifier::Not, json!("green")),
            (ValueType::String, "Ann", Modifier::Exact, json!("Ann")),
            (ValueType::String, "ann", Modifier::Exact, json!("Ann")),
            (ValueType::String, "nn", Modifier::Contains, json!("Ann")),
            (
                ValueType::Date,
                "ne1980",
                Modifier::None,
                json!("1980-04-01"),
            ),
            (
                ValueType::Date,
                "ne1990",
                Modifier::None,
                json!("1980-04-01"),
            ),
            (ValueType::Number, "0.4", Modifier::None, json!(0.42)),
        ];
        for (value_type, raw, modifier, element) in cases {
            let values = vec![
                crate::search::modifier::value_of(modifier, *value_type, raw)
                    .expect("the value parses under the modifier"),
            ];
            let target = Target::path(["held"]);
            let body = json!({"held": element});
            let held = indexed("held", target.clone(), modifier.clone(), values.clone());
            let over_body = Filter {
                name: "held".to_owned(),
                target,
                modifier: modifier.clone(),
                values,
                index: None,
                exempt: Vec::new(),
            };
            assert_eq!(
                held.matches_indexed(std::slice::from_ref(element)),
                over_body.matches(&id(), &updated(), &body),
                "{value_type:?} {raw} {modifier:?} {element}"
            );
        }
    }

    #[test]
    fn an_indexed_parameter_is_missing_when_it_extracted_nothing() {
        let target = Target::path(["held"]);
        let present = vec![SearchValue::Missing(false)];
        let absent = vec![SearchValue::Missing(true)];
        let none: &[Value] = &[];
        assert!(
            indexed("held", target.clone(), Modifier::Missing, absent.clone())
                .matches_indexed(none)
        );
        assert!(!indexed("held", target.clone(), Modifier::Missing, absent)
            .matches_indexed(&[json!("amber")]));
        assert!(
            indexed("held", target.clone(), Modifier::Missing, present.clone())
                .matches_indexed(&[json!("amber")])
        );
        assert!(!indexed("held", target, Modifier::Missing, present).matches_indexed(none));
    }

    #[test]
    fn a_sort_key_projects_the_value_it_orders_by() {
        let body = json!({"name": [{"family": "Ann"}], "birthDate": "1980-04-01"});
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
            sort_value(&Target::path(["birthDate"]), &id(), &updated(), &body),
            SortValue::Text("1980-04-01".to_owned())
        );
        assert_eq!(
            sort_value(
                &Target::path(["deceasedDateTime"]),
                &id(),
                &updated(),
                &body
            ),
            SortValue::Missing
        );
    }

    #[test]
    fn a_resource_with_no_value_for_the_key_sorts_after_every_other() {
        let text = SortValue::Text("a".to_owned());
        let instant = SortValue::Instant(updated().key());
        for held in [&text, &instant] {
            assert!(SortValue::Missing > *held, "{held:?}");
            assert!(*held < SortValue::Missing, "{held:?}");
        }
        assert_eq!(
            SortValue::Missing.cmp(&SortValue::Missing),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn keys_that_tie_leave_a_later_key_to_decide() {
        let first = json!({"name": [{"family": "Ann"}], "birthDate": "1980-04-01"});
        let second = json!({"name": [{"family": "ann"}], "birthDate": "1975-02-02"});
        let family = Target::path(["name.family"]);
        let birth = Target::path(["birthDate"]);
        let left = ResourceId::parse("a").unwrap();
        let right = ResourceId::parse("b").unwrap();
        assert_eq!(
            sort_value(&family, &left, &updated(), &first),
            sort_value(&family, &right, &updated(), &second)
        );
        assert!(
            sort_value(&birth, &right, &updated(), &second)
                < sort_value(&birth, &left, &updated(), &first)
        );
    }

    #[test]
    fn instants_order_chronologically() {
        let early = FhirInstant::parse("2020-01-01T00:00:00Z").unwrap();
        let late = FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
        assert!(SortValue::Instant(early.key()) < SortValue::Instant(late.key()));
    }

    #[test]
    fn a_code_set_yields_the_codes_it_selects_and_no_code_it_excludes() {
        let body = json!({
            "resourceType": "ValueSet",
            "compose": {
                "include": [{"system": "urn:s", "concept": [{"code": "a"}, {"code": "b"}]}],
                "exclude": [{"system": "urn:s", "concept": [{"code": "b"}]}]
            }
        });
        let codes = code_set(&body);
        assert_eq!(codes, vec![coded(Some("urn:s"), "a")]);
        assert!(code_set(&json!({})).is_empty());
    }

    #[test]
    fn a_nested_expansion_is_read_to_its_full_depth() {
        let body = json!({
            "resourceType": "ValueSet",
            "expansion": {"contains": [{
                "system": "urn:t",
                "code": "top",
                "contains": [
                    {"system": "urn:t", "code": "mid", "contains": [{"system": "urn:t", "code": "leaf"}]},
                    {"display": "an abstract grouping with no code"}
                ]
            }]}
        });
        let codes = code_set(&body);
        for code in ["top", "mid", "leaf"] {
            assert!(codes.contains(&coded(Some("urn:t"), code)), "{code}");
        }
        assert_eq!(codes.len(), 3);
    }

    #[test]
    fn every_version_reports_the_parameters_it_defines_but_cannot_answer() {
        for version in FhirVersion::ALL {
            let held = unsupported(version);
            for name in ["_content", "_filter", "_query"] {
                assert!(held.contains(&name), "{version:?} {name}");
            }
            for answered in [
                "_text",
                "_id",
                "_lastUpdated",
                "_profile",
                "_tag",
                "_security",
            ] {
                assert!(!held.contains(&answered), "{version:?} {answered}");
            }
        }
    }

    #[test]
    fn a_text_search_parameter_matches_the_words_of_the_narrative() {
        let id = id();
        let updated = updated();
        let body = json!({
            "resourceType": "Patient",
            "id": "p-1",
            "text": {"status": "generated", "div": "<div><p>fever and <b>chills</b></p></div>"}
        });
        let wanted = Filter::new(
            "_text",
            Target::path(["text"]),
            vec![SearchValue::parse(ValueType::String, "chills OR sweats").unwrap()],
        );
        assert!(wanted.matches(&id, &updated, &body));
        let absent = Filter::new(
            "_text",
            Target::path(["text"]),
            vec![SearchValue::parse(ValueType::String, "rash").unwrap()],
        );
        assert!(!absent.matches(&id, &updated, &body));
        let both = Filter::new(
            "_text",
            Target::path(["text"]),
            vec![SearchValue::parse(ValueType::String, "fever AND chills").unwrap()],
        );
        assert!(both.matches(&id, &updated, &body));
        let one = Filter::new(
            "_text",
            Target::path(["text"]),
            vec![SearchValue::parse(ValueType::String, "fever AND rash").unwrap()],
        );
        assert!(!one.matches(&id, &updated, &body));
    }
}
