use fhir_core::search::{lookup, Filter, SearchValue};
use fhir_core::{Error, ResourceEnvelope, ResourceId, ResourceType};
use fhir_store::{SearchPage, SearchQuery};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::query::pairs;

const CONTROL: [&str; 9] = [
    "_hardDelete",
    "_format",
    "_pretty",
    "_summary",
    "_count",
    "_sort",
    "_elements",
    "_total",
    "ct",
];

pub fn parse_query(
    base_type: Option<ResourceType>,
    raw: Option<&str>,
) -> Result<SearchQuery, Error> {
    let mut query = SearchQuery {
        types: base_type.into_iter().collect(),
        ..SearchQuery::default()
    };
    for (name, value) in pairs(raw) {
        if CONTROL.contains(&name.as_str()) {
            continue;
        }
        match name.as_str() {
            "_type" if base_type.is_none() => query.types = types(&value)?,
            "_list" => query.list = Some(list_id(&value)?),
            _ => query.filters.push(filter(base_type, &name, &value)?),
        }
    }
    Ok(query)
}

pub fn search_bundle(base: &str, self_url: &str, page: &SearchPage) -> Vec<u8> {
    let links = vec![serde_json::json!({ "relation": "self", "url": self_url })];
    let mut bundle = Map::new();
    bundle.insert("resourceType".to_owned(), Value::String("Bundle".to_owned()));
    bundle.insert("id".to_owned(), Value::String(Uuid::new_v4().to_string()));
    bundle.insert("type".to_owned(), Value::String("searchset".to_owned()));
    if let Some(total) = page.total {
        bundle.insert("total".to_owned(), Value::from(total));
    }
    bundle.insert("link".to_owned(), Value::Array(links));
    if !page.entries.is_empty() {
        let entries: Vec<Value> = page.entries.iter().map(|found| entry(base, found)).collect();
        bundle.insert("entry".to_owned(), Value::Array(entries));
    }
    serde_json::to_vec(&Value::Object(bundle)).expect("search bundle is serializable")
}

fn entry(base: &str, envelope: &ResourceEnvelope) -> Value {
    let resource_type = envelope.resource_type().as_str().to_owned();
    let id = envelope.id().as_str().to_owned();
    let mut entry = Map::new();
    entry.insert("fullUrl".to_owned(), Value::String(format!("{base}/{resource_type}/{id}")));
    if let Ok(resource) = serde_json::from_slice::<Value>(envelope.raw()) {
        entry.insert("resource".to_owned(), resource);
    }
    entry.insert("search".to_owned(), serde_json::json!({ "mode": "match" }));
    Value::Object(entry)
}

fn types(raw: &str) -> Result<Vec<ResourceType>, Error> {
    raw.split(',')
        .filter(|part| !part.is_empty())
        .map(str::parse::<ResourceType>)
        .collect()
}

fn list_id(raw: &str) -> Result<ResourceId, Error> {
    if raw.starts_with('$') {
        return Err(Error::UnsupportedParameter(format!("_list {raw:?}")));
    }
    ResourceId::parse(raw.rsplit('/').next().unwrap_or(raw))
}

fn filter(
    base_type: Option<ResourceType>,
    name: &str,
    raw: &str,
) -> Result<Filter, Error> {
    let def = lookup(base_type, name)
        .ok_or_else(|| Error::UnsupportedParameter(format!("{name:?}")))?;
    let values = raw
        .split(',')
        .map(|part| SearchValue::parse(def.value_type, part))
        .collect::<Result<Vec<SearchValue>, Error>>()?;
    if values.is_empty() {
        return Err(Error::InvalidParameter(format!("{name:?} has no value")));
    }
    Ok(Filter {
        name: name.to_owned(),
        target: def.target,
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patient() -> Option<ResourceType> {
        Some("Patient".parse().unwrap())
    }

    #[test]
    fn a_typed_search_without_parameters_selects_the_type() {
        let query = parse_query(patient(), None).unwrap();
        assert_eq!(query.types.len(), 1);
        assert!(query.is_unconditional());
    }

    #[test]
    fn control_parameters_carry_no_selection() {
        let query = parse_query(patient(), Some("_format=json&_count=5&ct=ab")).unwrap();
        assert!(query.is_unconditional());
    }

    #[test]
    fn common_parameters_become_filters() {
        let query = parse_query(patient(), Some("_id=pt-1&_tag=urn:t|a")).unwrap();
        assert_eq!(query.filters.len(), 2);
        assert_eq!(query.filters[0].values.len(), 1);
    }

    #[test]
    fn a_comma_separated_value_is_a_set_of_alternatives() {
        let query = parse_query(patient(), Some("_id=a,b,c")).unwrap();
        assert_eq!(query.filters[0].values.len(), 3);
    }

    #[test]
    fn an_unimplemented_parameter_is_rejected() {
        for raw in ["nonesuch=1", "_include=Patient:link", "name:exact=Ann", "subject.name=Ann"] {
            let error = parse_query(patient(), Some(raw)).unwrap_err();
            assert!(matches!(error, Error::UnsupportedParameter(_)), "{raw} gave {error:?}");
        }
    }

    #[test]
    fn a_malformed_value_is_rejected() {
        let error = parse_query(patient(), Some("_lastUpdated=whenever")).unwrap_err();
        assert!(matches!(error, Error::InvalidParameter(_)));
    }

    #[test]
    fn the_type_parameter_applies_to_a_search_across_every_type() {
        let query = parse_query(None, Some("_type=Patient,Observation")).unwrap();
        assert_eq!(query.types.len(), 2);
        let scoped = parse_query(patient(), Some("_type=Observation")).unwrap_err();
        assert!(matches!(scoped, Error::UnsupportedParameter(_)));
        assert!(parse_query(None, Some("_type=Nonesuch")).is_err());
    }

    #[test]
    fn the_list_parameter_names_a_list_resource() {
        let query = parse_query(patient(), Some("_list=ls-1")).unwrap();
        assert_eq!(query.list.map(|id| id.as_str().to_owned()), Some("ls-1".to_owned()));
        assert_eq!(
            parse_query(patient(), Some("_list=List/ls-2")).unwrap().list.map(|id| id.as_str().to_owned()),
            Some("ls-2".to_owned())
        );
        let current = parse_query(patient(), Some("_list=$current-problems")).unwrap_err();
        assert!(matches!(current, Error::UnsupportedParameter(_)));
    }
}
