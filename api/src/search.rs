use fhir_core::search::{
    Chain, ChainDirection, Criterion, Filter, Include, IncludeDirection, Modifier, Registry,
    SearchValue,
};
use fhir_core::{Error, ResourceEnvelope, ResourceId, ResourceType};
use fhir_store::{SearchPage, SearchQuery, SortDirection, SortKey, TotalMode};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::history::Summary;
use crate::query::{pairs, param};
use crate::token::{decode, encode, scope, scope_of, with_token};

pub(crate) const CONTROL: [&str; 9] = [
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

fn base_of(name: &str) -> &str {
    name.split_once(':').map(|(base, _)| base).unwrap_or(name)
}

pub fn parse_query(
    registry: &Registry,
    base_type: Option<ResourceType>,
    raw: Option<&str>,
) -> Result<SearchQuery, Error> {
    let mut query = SearchQuery {
        types: base_type.into_iter().collect(),
        ..SearchQuery::default()
    };
    for (name, value) in pairs(raw) {
        if value.is_empty() {
            return Err(Error::UnsupportedParameter(format!(
                "{name:?} with an empty value"
            )));
        }
        if fhir_core::search::unsupported(registry.fhir_version()).contains(&base_of(&name)) {
            return Err(Error::UnsupportedParameter(format!("{name:?}")));
        }
        if CONTROL.contains(&name.as_str()) {
            continue;
        }
        match name.as_str() {
            "_type" if base_type.is_none() => query.types = types(&value)?,
            "_list" => query.list = Some(list_id(&value)?),
            spelled if spelled == "_include" || spelled.starts_with("_include:") => {
                query.includes.push(inclusion(
                    registry,
                    &name,
                    &value,
                    IncludeDirection::Forward,
                )?)
            }
            spelled if spelled == "_revinclude" || spelled.starts_with("_revinclude:") => {
                query.includes.push(inclusion(
                    registry,
                    &name,
                    &value,
                    IncludeDirection::Reverse,
                )?)
            }
            _ => match criterion(registry, base_type, &name, &value)? {
                Criterion::Direct(found) => query.filters.push(found),
                Criterion::Linked(chain) => query.chains.push(chain),
            },
        }
    }
    Ok(query)
}

const DEFAULT_COUNT: usize = 20;
const MAX_COUNT: usize = 100;

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: SearchQuery,
    pub summary: Summary,
    pub elements: Vec<String>,
}

impl SearchRequest {
    pub fn parse(
        registry: &Registry,
        base_type: Option<ResourceType>,
        raw: Option<&str>,
    ) -> Result<SearchRequest, Error> {
        let mut query = parse_query(registry, base_type, raw)?;
        let control = ResultControl::parse(raw)?;
        query.sort = sort_of(registry, base_type, raw)?;
        query.total = control.total;
        query.count = control.count;
        query.offset = control.offset;
        Ok(SearchRequest {
            query,
            summary: control.summary,
            elements: control.elements,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultControl {
    pub count: usize,
    pub offset: usize,
    pub summary: Summary,
    pub elements: Vec<String>,
    pub total: TotalMode,
}

impl ResultControl {
    pub fn parse(raw: Option<&str>) -> Result<ResultControl, Error> {
        let summary = summary_of(raw)?;
        let elements = param(raw, "_elements")
            .map(|text| {
                text.split(',')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Ok(ResultControl {
            count: match summary {
                Summary::Count => 0,
                _ => count_of(raw)?,
            },
            offset: match param(raw, "ct") {
                Some(text) => decode(&text, &scope(raw))?,
                None => 0,
            },
            summary,
            elements,
            total: match summary {
                Summary::Count => TotalMode::Accurate,
                _ => total_of(raw)?,
            },
        })
    }
}

fn summary_of(raw: Option<&str>) -> Result<Summary, Error> {
    match param(raw, "_summary").as_deref() {
        None | Some("false") | Some("data") => Ok(Summary::Full),
        Some("true") => Ok(Summary::Metadata),
        Some("text") => Ok(Summary::Text),
        Some("count") => Ok(Summary::Count),
        Some(other) => Err(Error::UnsupportedParameter(format!("_summary {other:?}"))),
    }
}

fn total_of(raw: Option<&str>) -> Result<TotalMode, Error> {
    match param(raw, "_total").as_deref() {
        None | Some("accurate") => Ok(TotalMode::Accurate),
        Some("estimate") => Ok(TotalMode::Estimate),
        Some("none") => Ok(TotalMode::None),
        Some(other) => Err(Error::UnsupportedParameter(format!("_total {other:?}"))),
    }
}

fn count_of(raw: Option<&str>) -> Result<usize, Error> {
    match param(raw, "_count") {
        Some(text) => Ok(text
            .parse::<usize>()
            .map_err(|_| Error::InvalidParameter(format!("_count {text:?}")))?
            .min(MAX_COUNT)),
        None => Ok(DEFAULT_COUNT),
    }
}

fn sort_of(
    registry: &Registry,
    base_type: Option<ResourceType>,
    raw: Option<&str>,
) -> Result<Vec<SortKey>, Error> {
    let Some(text) = param(raw, "_sort") else {
        return Ok(Vec::new());
    };
    text.split(',')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (direction, name) = match part.strip_prefix('-') {
                Some(rest) => (SortDirection::Descending, rest),
                None => (SortDirection::Ascending, part),
            };
            let def = registry
                .searchable(base_type, name)?
                .filter(|def| def.sortable)
                .ok_or_else(|| Error::UnsupportedParameter(format!("_sort {name:?}")))?;
            Ok(SortKey {
                name: name.to_owned(),
                target: def.target.clone(),
                direction,
            })
        })
        .collect()
}

pub fn search_bundle(
    base: &str,
    self_url: &str,
    page: &SearchPage,
    summary: Summary,
    elements: &[String],
) -> Vec<u8> {
    bundle_of(base, self_url, page, summary, elements, "match")
}

pub fn includes_bundle(
    base: &str,
    self_url: &str,
    page: &SearchPage,
    summary: Summary,
    elements: &[String],
) -> Vec<u8> {
    bundle_of(base, self_url, page, summary, elements, "include")
}

fn bundle_of(
    base: &str,
    self_url: &str,
    page: &SearchPage,
    summary: Summary,
    elements: &[String],
    mode: &str,
) -> Vec<u8> {
    let mut links = vec![serde_json::json!({ "relation": "self", "url": self_url })];
    let consumed = page.offset + page.entries.len();
    if page.total.is_some_and(|total| consumed < total) && !page.entries.is_empty() {
        links.push(serde_json::json!({
            "relation": "next",
            "url": with_token(self_url, &encode(consumed, &scope_of(self_url))),
        }));
    }
    let mut bundle = Map::new();
    bundle.insert(
        "resourceType".to_owned(),
        Value::String("Bundle".to_owned()),
    );
    bundle.insert("id".to_owned(), Value::String(Uuid::new_v4().to_string()));
    bundle.insert("type".to_owned(), Value::String("searchset".to_owned()));
    if let Some(total) = page.total {
        bundle.insert("total".to_owned(), Value::from(total));
    }
    bundle.insert("link".to_owned(), Value::Array(links));
    let mut rendered: Vec<Value> = page
        .entries
        .iter()
        .map(|found| entry(base, found, mode, summary, elements))
        .collect();
    rendered.extend(
        page.included
            .iter()
            .map(|found| entry(base, found, "include", summary, elements)),
    );
    if !rendered.is_empty() {
        bundle.insert("entry".to_owned(), Value::Array(rendered));
    }
    serde_json::to_vec(&Value::Object(bundle)).expect("search bundle is serializable")
}

fn entry(
    base: &str,
    envelope: &ResourceEnvelope,
    mode: &str,
    summary: Summary,
    elements: &[String],
) -> Value {
    let resource_type = envelope.resource_type().as_str().to_owned();
    let id = envelope.id().as_str().to_owned();
    let mut entry = Map::new();
    entry.insert(
        "fullUrl".to_owned(),
        Value::String(format!("{base}/{resource_type}/{id}")),
    );
    if let Some(resource) = resource_of(envelope, summary, elements) {
        entry.insert("resource".to_owned(), resource);
    }
    entry.insert("search".to_owned(), serde_json::json!({ "mode": mode }));
    Value::Object(entry)
}

fn resource_of(
    envelope: &ResourceEnvelope,
    summary: Summary,
    elements: &[String],
) -> Option<Value> {
    let rendered: Value = match summary {
        Summary::Count => return None,
        Summary::Metadata => serde_json::from_slice(&envelope.to_metadata_json()).ok()?,
        Summary::Text => narrowed(envelope, &["text".to_owned()])?,
        Summary::Full if elements.is_empty() => return serde_json::from_slice(envelope.raw()).ok(),
        Summary::Full => narrowed(envelope, elements)?,
    };
    Some(subsetted(rendered))
}

pub(crate) fn narrowed(envelope: &ResourceEnvelope, elements: &[String]) -> Option<Value> {
    let value: Value = serde_json::from_slice(envelope.raw()).ok()?;
    let source = value.as_object()?;
    let mut kept = Map::new();
    for name in ["resourceType", "id", "meta"] {
        if let Some(found) = source.get(name) {
            kept.insert(name.to_owned(), found.clone());
        }
    }
    for name in elements {
        if let Some(found) = source.get(name.as_str()) {
            kept.insert(name.clone(), found.clone());
        }
    }
    Some(Value::Object(kept))
}

fn subsetted(mut value: Value) -> Value {
    let tag = serde_json::json!({
        "system": "http://terminology.hl7.org/CodeSystem/v3-ObservationValue",
        "code": "SUBSETTED",
    });
    if let Some(object) = value.as_object_mut() {
        let meta = object
            .entry("meta".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(meta) = meta.as_object_mut() {
            match meta
                .entry("tag".to_owned())
                .or_insert_with(|| Value::Array(Vec::new()))
            {
                Value::Array(tags) => tags.push(tag),
                other => *other = Value::Array(vec![tag]),
            }
        }
    }
    value
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
    registry: &Registry,
    base_type: Option<ResourceType>,
    name: &str,
    raw: &str,
) -> Result<Filter, Error> {
    let (base, modifier) = split_modifier(name)?;
    let def = registry
        .searchable(base_type, base)?
        .ok_or_else(|| Error::UnsupportedParameter(format!("{name:?}")))?;
    if matches!(def.target, fhir_core::search::Target::Collection) {
        return collection(name, &def, &modifier, raw);
    }
    if !modifier.applies_in(def.value_type, registry.fhir_version()) {
        return Err(Error::UnsupportedParameter(format!("{name:?}")));
    }
    let values = if modifier == Modifier::Text
        && def.value_type == fhir_core::search::ValueType::Reference
    {
        vec![SearchValue::parse(
            fhir_core::search::ValueType::Reference,
            raw,
        )?]
    } else if base == "_text" {
        if modifier == Modifier::None {
            fhir_core::search::text::text_query(raw)?;
        }
        vec![def.value_with(&modifier, raw)?]
    } else {
        raw.split(',')
            .map(|part| def.value_with(&modifier, part))
            .collect::<Result<Vec<SearchValue>, Error>>()?
    };
    if values.is_empty() {
        return Err(Error::InvalidParameter(format!("{name:?} has no value")));
    }
    Ok(Filter {
        name: name.to_owned(),
        target: def.target.clone(),
        modifier,
        values,
        index: def.url.clone(),
    })
}

fn collection(
    name: &str,
    def: &std::sync::Arc<fhir_core::search::ParamDef>,
    modifier: &Modifier,
    raw: &str,
) -> Result<Filter, Error> {
    let negated = match modifier {
        Modifier::None => false,
        Modifier::Not => true,
        _ => return Err(Error::UnsupportedParameter(format!("{name:?}"))),
    };
    let values = raw
        .split(',')
        .map(|part| SearchValue::parse(def.value_type, part))
        .collect::<Result<Vec<SearchValue>, Error>>()?;
    if values.is_empty() {
        return Err(Error::InvalidParameter(format!("{name:?} has no value")));
    }
    Ok(Filter {
        name: name.to_owned(),
        target: fhir_core::search::Target::Collection,
        modifier: match negated {
            true => Modifier::Not,
            false => Modifier::None,
        },
        values,
        index: def.url.clone(),
    })
}

fn inclusion(
    registry: &Registry,
    name: &str,
    raw: &str,
    direction: IncludeDirection,
) -> Result<Include, Error> {
    let iterate = match name.split_once(':') {
        None => false,
        Some((_, "iterate")) | Some((_, "recurse")) => true,
        Some((_, other)) => return Err(Error::UnsupportedParameter(format!("{name:?} {other:?}"))),
    };
    let spelling = format!("{name}={raw}");
    let unsupported = || Error::UnsupportedParameter(format!("{spelling:?}"));
    let mut parts = raw.split(':');
    let head = parts.next().unwrap_or_default();
    if head == "*" {
        if parts.next().is_some() {
            return Err(unsupported());
        }
        return Ok(Include {
            name: spelling,
            source: None,
            paths: Vec::new(),
            target: None,
            direction,
            iterate,
        });
    }
    let source: ResourceType = head.parse()?;
    let param = parts.next().ok_or_else(unsupported)?;
    let target = match parts.next() {
        Some(text) => Some(text.parse::<ResourceType>()?),
        None => None,
    };
    if parts.next().is_some() {
        return Err(unsupported());
    }
    let paths: Vec<String> = if param == "*" {
        registry
            .references(source)
            .iter()
            .flat_map(|def| def.paths())
            .collect()
    } else {
        let def = registry
            .searchable(Some(source), param)?
            .ok_or_else(unsupported)?;
        if def.value_type != fhir_core::search::ValueType::Reference {
            return Err(unsupported());
        }
        def.paths()
    };
    if paths.is_empty() {
        return Err(unsupported());
    }
    Ok(Include {
        name: spelling,
        source: Some(source),
        paths,
        target,
        direction,
        iterate,
    })
}

fn criterion(
    registry: &Registry,
    base_type: Option<ResourceType>,
    name: &str,
    raw: &str,
) -> Result<Criterion, Error> {
    if let Some(rest) = name.strip_prefix("_has:") {
        return reverse(registry, name, rest, raw);
    }
    match name.split_once('.') {
        Some((head, tail)) => forward(registry, base_type, name, head, tail, raw),
        None => Ok(Criterion::Direct(filter(registry, base_type, name, raw)?)),
    }
}

fn link_of(
    registry: &Registry,
    base_type: Option<ResourceType>,
    name: &str,
    spelling: &str,
) -> Result<
    (
        std::sync::Arc<fhir_core::search::ParamDef>,
        Option<ResourceType>,
    ),
    Error,
> {
    let (param, wanted) = match spelling.split_once(':') {
        Some((param, text)) => (param, Some(text.parse::<ResourceType>()?)),
        None => (spelling, None),
    };
    let def = registry
        .searchable(base_type, param)?
        .ok_or_else(|| Error::UnsupportedParameter(format!("{name:?}")))?;
    if def.value_type != fhir_core::search::ValueType::Reference {
        return Err(Error::UnsupportedParameter(format!(
            "{name:?} does not follow a reference"
        )));
    }
    Ok((def, wanted))
}

fn forward(
    registry: &Registry,
    base_type: Option<ResourceType>,
    name: &str,
    head: &str,
    tail: &str,
    raw: &str,
) -> Result<Criterion, Error> {
    let (def, wanted) = link_of(registry, base_type, name, head)?;
    let candidates: Vec<ResourceType> = match wanted {
        Some(one) => vec![one],
        None => def
            .targets
            .iter()
            .map(|text| text.parse::<ResourceType>())
            .collect::<Result<Vec<ResourceType>, Error>>()?,
    };
    let mut types = Vec::new();
    let mut next: Option<Criterion> = None;
    for candidate in candidates {
        match criterion(registry, Some(candidate), tail, raw) {
            Ok(found) => {
                match &next {
                    None => next = Some(found),
                    Some(existing) if *existing == found => {}
                    Some(_) => {
                        return Err(Error::UnsupportedParameter(format!(
                            "{name:?} needs the type of the resource it chains to"
                        )))
                    }
                }
                types.push(candidate);
            }
            Err(Error::UnsupportedParameter(_)) => continue,
            Err(other) => return Err(other),
        }
    }
    match next {
        Some(next) => Ok(Criterion::Linked(Chain {
            name: name.to_owned(),
            
            
            link: head.split_once(':').map_or(head, |(param, _)| param).to_owned(),
            target: def.target.clone(),
            types,
            direction: ChainDirection::Forward,
            next: Box::new(next),
        })),
        None => Err(Error::UnsupportedParameter(format!("{name:?}"))),
    }
}

fn reverse(registry: &Registry, name: &str, rest: &str, raw: &str) -> Result<Criterion, Error> {
    let mut parts = rest.splitn(3, ':');
    let spelled = (parts.next(), parts.next(), parts.next());
    let (source, link, remainder) = match spelled {
        (Some(source), Some(link), Some(remainder))
            if !source.is_empty() && !link.is_empty() && !remainder.is_empty() =>
        {
            (source, link, remainder)
        }
        _ => {
            return Err(Error::UnsupportedParameter(format!(
                "{name:?} needs a type, a reference and a parameter"
            )))
        }
    };
    let source_type: ResourceType = source.parse()?;
    let (def, _) = link_of(registry, Some(source_type), name, link)?;
    let next = criterion(registry, Some(source_type), remainder, raw)?;
    Ok(Criterion::Linked(Chain {
        name: name.to_owned(),
        link: link.to_owned(),
        target: def.target.clone(),
        types: vec![source_type],
        direction: ChainDirection::Reverse,
        next: Box::new(next),
    }))
}

fn split_modifier(name: &str) -> Result<(&str, Modifier), Error> {
    match name.split_once(':') {
        Some((base, text)) => Ok((base, text.parse::<Modifier>()?)),
        None => Ok((name, Modifier::None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::search::ValueType;

    fn patient() -> Option<ResourceType> {
        Some("Patient".parse().unwrap())
    }

    #[test]
    fn a_typed_search_without_parameters_selects_the_type() {
        let query = parse_query(&Registry::new(), patient(), None).unwrap();
        assert_eq!(query.types.len(), 1);
        assert!(query.is_unconditional());
    }

    #[test]
    fn control_parameters_carry_no_selection() {
        let query = parse_query(
            &Registry::new(),
            patient(),
            Some("_format=json&_count=5&ct=ab"),
        )
        .unwrap();
        assert!(query.is_unconditional());
    }

    #[test]
    fn common_parameters_become_filters() {
        let query =
            parse_query(&Registry::new(), patient(), Some("_id=pt-1&_tag=urn:t|a")).unwrap();
        assert_eq!(query.filters.len(), 2);
        assert_eq!(query.filters[0].values.len(), 1);
    }

    #[test]
    fn a_comma_separated_value_is_a_set_of_alternatives() {
        let query = parse_query(&Registry::new(), patient(), Some("_id=a,b,c")).unwrap();
        assert_eq!(query.filters[0].values.len(), 3);
    }

    #[test]
    fn an_unimplemented_parameter_is_rejected() {
        for raw in [
            "nonesuch=1",
            "_include=Patient:nonesuch",
            "subject.name=Ann",
        ] {
            let error = parse_query(&Registry::new(), patient(), Some(raw)).unwrap_err();
            assert!(
                matches!(error, Error::UnsupportedParameter(_)),
                "{raw} gave {error:?}"
            );
        }
    }

    #[test]
    fn an_empty_value_and_the_unused_full_text_parameters_are_unsupported() {
        for raw in [
            "_id=",
            "name=",
            "_content=x",
            "_filter=name%20eq%20a",
            "_query=x",
        ] {
            let error = parse_query(&Registry::new(), patient(), Some(raw)).unwrap_err();
            assert!(
                matches!(error, Error::UnsupportedParameter(_)),
                "{raw} gave {error:?}"
            );
        }
        assert!(matches!(
            SearchRequest::parse(&Registry::new(), patient(), Some("_sort=")).unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
    }

    #[test]
    fn a_text_search_reads_the_narrative_as_a_single_value() {
        let query = parse_query(
            &Registry::new(),
            patient(),
            Some("_text=(bone%20OR%20liver)%20AND%20metastases"),
        )
        .unwrap();
        assert_eq!(query.filters.len(), 1);
        let filter = &query.filters[0];
        assert_eq!(filter.name, "_text");
        assert_eq!(filter.modifier, Modifier::None);
        assert_eq!(
            filter.values,
            vec![SearchValue::parse(ValueType::String, "(bone OR liver) AND metastases").unwrap()]
        );
    }

    #[test]
    fn a_malformed_text_search_value_is_rejected() {
        for raw in ["_text=%28bone", "_text=bone%29"] {
            let error = parse_query(&Registry::new(), patient(), Some(raw)).unwrap_err();
            assert!(
                matches!(error, Error::InvalidParameter(_)),
                "{raw} gave {error:?}"
            );
        }
    }

    #[test]
    fn a_malformed_value_is_rejected() {
        let error =
            parse_query(&Registry::new(), patient(), Some("_lastUpdated=whenever")).unwrap_err();
        assert!(matches!(error, Error::InvalidParameter(_)));
    }

    #[test]
    fn the_type_parameter_applies_to_a_search_across_every_type() {
        let query = parse_query(&Registry::new(), None, Some("_type=Patient,Observation")).unwrap();
        assert_eq!(query.types.len(), 2);
        let scoped =
            parse_query(&Registry::new(), patient(), Some("_type=Observation")).unwrap_err();
        assert!(matches!(scoped, Error::UnsupportedParameter(_)));
        assert!(parse_query(&Registry::new(), None, Some("_type=Nonesuch")).is_err());
    }

    #[test]
    fn the_list_parameter_names_a_list_resource() {
        let query = parse_query(&Registry::new(), patient(), Some("_list=ls-1")).unwrap();
        assert_eq!(
            query.list.map(|id| id.as_str().to_owned()),
            Some("ls-1".to_owned())
        );
        assert_eq!(
            parse_query(&Registry::new(), patient(), Some("_list=List/ls-2"))
                .unwrap()
                .list
                .map(|id| id.as_str().to_owned()),
            Some("ls-2".to_owned())
        );
        let current =
            parse_query(&Registry::new(), patient(), Some("_list=$current-problems")).unwrap_err();
        assert!(matches!(current, Error::UnsupportedParameter(_)));
    }

    #[test]
    fn result_control_defaults_apply_when_nothing_is_asked_for() {
        let request = SearchRequest::parse(&Registry::new(), patient(), None).unwrap();
        assert_eq!(request.summary, Summary::Full);
        assert_eq!(request.query.count, DEFAULT_COUNT);
        assert_eq!(request.query.total, TotalMode::Accurate);
        assert!(request.query.sort.is_empty());
        assert!(request.elements.is_empty());
    }

    #[test]
    fn count_is_capped_and_a_count_summary_drops_entries() {
        assert_eq!(
            SearchRequest::parse(&Registry::new(), patient(), Some("_count=5000"))
                .unwrap()
                .query
                .count,
            MAX_COUNT
        );
        let counted = SearchRequest::parse(
            &Registry::new(),
            patient(),
            Some("_summary=count&_total=none"),
        )
        .unwrap();
        assert_eq!(counted.query.count, 0);
        assert_eq!(counted.query.total, TotalMode::Accurate);
    }

    #[test]
    fn a_sort_key_carries_its_direction() {
        let request =
            SearchRequest::parse(&Registry::new(), patient(), Some("_sort=-_lastUpdated,_id"))
                .unwrap();
        assert_eq!(request.query.sort[0].direction, SortDirection::Descending);
        assert_eq!(request.query.sort[1].name, "_id");
        assert_eq!(request.query.sort[1].direction, SortDirection::Ascending);
    }

    #[test]
    fn elements_are_split_on_commas() {
        let request =
            SearchRequest::parse(&Registry::new(), patient(), Some("_elements=active,gender"))
                .unwrap();
        assert_eq!(
            request.elements,
            vec!["active".to_owned(), "gender".to_owned()]
        );
    }

    #[test]
    fn a_narrative_summary_is_recognised() {
        assert_eq!(
            SearchRequest::parse(&Registry::new(), patient(), Some("_summary=text"))
                .unwrap()
                .summary,
            Summary::Text
        );
        assert_eq!(
            SearchRequest::parse(&Registry::new(), patient(), Some("_summary=data"))
                .unwrap()
                .summary,
            Summary::Full
        );
    }

    
    
    
    
    
    
    
    #[test]
    fn a_chain_keeps_the_link_apart_from_the_whole_spelling() {
        use fhir_core::search::{Chain, ChainDirection, Criterion};

        let held = |base: &str, raw: &str| -> Chain {
            let kind: Option<ResourceType> = Some(base.parse().unwrap());
            let query = parse_query(&Registry::new(), kind, Some(raw))
                .unwrap_or_else(|error| panic!("{raw} does not parse: {error}"));
            match query.chains.first().cloned() {
                Some(chain) => chain,
                None => panic!("{raw} produced no chain"),
            }
        };

        let forward = held("Observation", "subject._id=101");
        assert_eq!(forward.name, "subject._id");
        assert_eq!(forward.link, "subject", "the index is keyed by the link");
        assert_eq!(forward.direction, ChainDirection::Forward);

        
        
        let narrowed = held("Observation", "subject:Patient._id=101");
        assert_eq!(narrowed.link, "subject");

        let reverse = held("Patient", "_has:Observation:subject:status=final");
        assert_eq!(reverse.link, "subject");
        assert_eq!(reverse.direction, ChainDirection::Reverse);

        
        let deep = held("Observation", "subject.general-practitioner._id=7");
        assert_eq!(deep.link, "subject");
        match deep.next.as_ref() {
            Criterion::Linked(inner) => assert_eq!(inner.link, "general-practitioner"),
            Criterion::Direct(_) => panic!("the second link is not a chain"),
        }
    }
}
