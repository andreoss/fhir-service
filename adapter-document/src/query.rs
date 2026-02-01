use crate::expr::{all, array_for, Compiler};
use crate::record::{entries_of, envelope_of, pointers_of};
use crate::store::{faulted, DocumentStore};
use fhir_core::search::{
    select, Criterion, Filter, Include, IncludeDirection, Modifier, ParameterSpec, SearchValue,
    Target,
};
use fhir_core::{Error, ResourceEnvelope, ResourceType};
use fhir_store::index::{
    declared, logical, normalized, overflowed, parses, reference_of, rows_of, Rows, MAIN,
};
use fhir_store::{
    IndexFailure, IndexReport, Plan, PlanKey, SearchPage, SearchQuery, SortDirection, TotalMode,
};
use mongodb::bson::{doc, Bson, Document};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

const ROUNDS: usize = 5;
const ARRAYS: [&str; 7] = [
    "token",
    "text",
    "number",
    "date",
    "quantity",
    "reference",
    "uri",
];

fn body_of(envelope: &ResourceEnvelope) -> Result<Value, Error> {
    serde_json::from_slice(envelope.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

fn live() -> Document {
    doc! {"is_current": true, "is_deleted": false}
}

fn names_of(types: &[ResourceType]) -> Vec<Bson> {
    types
        .iter()
        .map(|kind| Bson::String(kind.as_str().to_owned()))
        .collect()
}

fn pointers(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| pointers(item, out)),
        Value::Object(map) => out.extend(
            map.get("reference")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn every_pointer(body: &Value, out: &mut Vec<String>) {
    match body {
        Value::Array(items) => items.iter().for_each(|item| every_pointer(item, out)),
        Value::Object(map) => {
            if let Some(text) = map.get("reference").and_then(Value::as_str) {
                out.push(text.to_owned());
            }
            map.values().for_each(|nested| every_pointer(nested, out));
        }
        Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn from_body(rule: &Include, envelope: &ResourceEnvelope) -> Result<Vec<String>, Error> {
    let body = body_of(envelope)?;
    let mut found = Vec::new();
    match rule.is_wildcard() {
        true => every_pointer(&body, &mut found),
        false => {
            for path in &rule.paths {
                for element in select(&body, path) {
                    pointers(element, &mut found);
                }
            }
        }
    }
    Ok(found)
}

fn linked(rule: &Include, held: &Document, envelope: &ResourceEnvelope) -> Result<Vec<String>, Error> {
    match rule.is_wildcard() || !held.contains_key("reference") {
        true => from_body(rule, envelope),
        false => Ok(pointers_of(held, &rule.name)),
    }
}

async fn code_set(store: &DocumentStore, url: &str) -> Result<Vec<SearchValue>, Error> {
    let mut filter = live();
    filter.insert("resource_type", "ValueSet");
    let found = store
        .listed(vec![doc! {"$match": filter}], "reading a code set")
        .await?;
    for held in &found {
        let envelope = envelope_of(held)?;
        let body = body_of(&envelope)?;
        if body.get("url").and_then(Value::as_str) == Some(url) {
            return Ok(fhir_core::search::code_set(&body));
        }
    }
    Err(Error::InvalidParameter(format!("code set {url:?} is unknown")))
}

async fn expanded(store: &DocumentStore, filter: &Filter) -> Result<Filter, Error> {
    let addresses = filter.code_sets();
    if addresses.is_empty() {
        return Ok(filter.clone());
    }
    let mut values = Vec::new();
    for address in addresses {
        values.extend(code_set(store, &address).await?);
    }
    Ok(filter.resolved(&values))
}

async fn members(store: &DocumentStore, id: &fhir_core::ResourceId) -> Result<Vec<String>, Error> {
    let mut filter = live();
    filter.insert("resource_id", id.as_str());
    let Some(held) = store.perhaps(filter, "reading list membership").await? else {
        return Ok(Vec::new());
    };
    let body = body_of(&envelope_of(&held)?)?;
    Ok(select(&body, "entry.item.reference")
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect())
}

fn driving(query: &SearchQuery) -> Option<&Filter> {
    query.filters.iter().find(|filter| {
        matches!(filter.target, Target::Path(_))
            && filter.modifier == Modifier::None
            && !filter.values.is_empty()
            && filter.values.iter().all(|value| !value.is_negated())
            && !filter
                .values
                .iter()
                .any(|value| matches!(value, SearchValue::Missing(_)))
    })
}

fn proposed(query: &SearchQuery) -> Plan {
    match driving(query) {
        Some(filter) => Plan::Indexed {
            parameter: filter.name.clone(),
        },
        None => Plan::Scan,
    }
}

fn drive(filter: &Filter) -> Option<Document> {
    let param = filter.index.clone().unwrap_or_else(|| filter.name.clone());
    let array = array_for(filter)?;
    Some(doc! {array: {"$elemMatch": {"param": param, "slot": MAIN}}})
}

fn base_of(query: &SearchQuery, plan: &Plan) -> Document {
    let mut base = live();
    if !query.types.is_empty() {
        base.insert("resource_type", doc! {"$in": names_of(&query.types)});
    }
    if plan.is_indexed() {
        if let Some(clause) = driving(query).and_then(drive) {
            base.extend(clause);
        }
    }
    base
}

async fn selection(
    store: &DocumentStore,
    query: &SearchQuery,
    compiler: &mut Compiler,
    stages: &mut Vec<Document>,
) -> Result<Vec<Bson>, Error> {
    let mut conditions: Vec<Bson> = Vec::new();
    if let Some(list) = &query.list {
        let found: Vec<Bson> = members(store, list)
            .await?
            .into_iter()
            .map(Bson::String)
            .collect();
        conditions.push(Bson::Document(doc! {"$in": [
            {"$concat": ["$resource_type", "/", "$resource_id"]},
            found,
        ]}));
    }
    if let Some(grant) = &query.grant {
        conditions.push(compiler.grant(grant)?);
    }
    if let Some(compartment) = &query.compartment {
        conditions.push(compiler.compartment(compartment));
    }
    for filter in &query.filters {
        let resolved = expanded(store, filter).await?;
        conditions.push(compiler.filter(&resolved)?);
    }
    for chain in &query.chains {
        let criterion = Criterion::Linked(chain.clone());
        conditions.push(compiler.criterion(&criterion, stages, query.grant.as_ref())?);
    }
    Ok(conditions)
}

async fn examined(store: &DocumentStore, query: &SearchQuery, plan: &Plan) -> Result<u64, Error> {
    let mut counted = live();
    if !query.types.is_empty() {
        counted.insert("resource_type", doc! {"$in": names_of(&query.types)});
    }
    if plan.is_indexed() {
        match driving(query).and_then(drive) {
            Some(clause) => counted.extend(clause),
            None => return Ok(0),
        }
    }
    store.counted(counted, "measuring a plan").await
}

fn ordering(query: &SearchQuery, pipeline: &mut Vec<Document>) {
    let mut order = Document::new();
    for (at, sort) in query.sort.iter().enumerate() {
        let name = format!("order{at}");
        pipeline.push(doc! {"$addFields": {&name: {"$ifNull": [
            {"$first": {"$map": {
                "input": {"$filter": {
                    "input": {"$ifNull": ["$sort", []]},
                    "as": "s",
                    "cond": {"$eq": ["$$s.param", &sort.name]},
                }},
                "as": "s",
                "in": "$$s.value",
            }}},
            "2",
        ]}}});
        order.insert(
            name,
            match sort.direction {
                SortDirection::Ascending => 1,
                SortDirection::Descending => -1,
            },
        );
    }
    order.insert("resource_id", 1);
    pipeline.push(doc! {"$sort": order});
}

async fn by_reference(
    store: &DocumentStore,
    query: &SearchQuery,
    texts: &[String],
    target: Option<ResourceType>,
) -> Result<Vec<Document>, Error> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let full: Vec<Bson> = texts
        .iter()
        .map(|text| Bson::String(normalized(text)))
        .collect();
    let bare: Vec<Bson> = texts
        .iter()
        .map(|text| Bson::String(logical(text)))
        .collect();
    let mut base = live();
    if let Some(kind) = target {
        base.insert("resource_type", kind.as_str());
    }
    let mut conditions = vec![Bson::Document(doc! {"$or": [
        {"$in": [{"$concat": ["$resource_type", "/", "$resource_id"]}, full]},
        {"$in": ["$resource_id", bare]},
    ]})];
    let mut compiler = Compiler::new();
    if let Some(grant) = &query.grant {
        conditions.push(compiler.grant(grant)?);
    }
    store
        .listed(
            vec![
                doc! {"$match": base},
                doc! {"$match": {"$expr": all(conditions)}},
            ],
            "reading linked resources",
        )
        .await
}

async fn pointing_at(
    store: &DocumentStore,
    query: &SearchQuery,
    rule: &Include,
    texts: &[String],
) -> Result<Vec<Document>, Error> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let full: Vec<Bson> = texts
        .iter()
        .map(|text| Bson::String(normalized(text)))
        .collect();
    let bare: Vec<Bson> = texts
        .iter()
        .map(|text| Bson::String(logical(text)))
        .collect();
    let mut base = live();
    if let Some(source) = rule.source {
        base.insert("resource_type", source.as_str());
    }
    base.insert(
        "reference",
        doc! {"$elemMatch": {
            "param": &rule.name,
            "slot": MAIN,
            "$or": [{"pointer": {"$in": full}}, {"logical": {"$in": bare}}],
        }},
    );
    let mut conditions: Vec<Bson> = Vec::new();
    let mut compiler = Compiler::new();
    if let Some(grant) = &query.grant {
        conditions.push(compiler.grant(grant)?);
    }
    store
        .listed(
            vec![
                doc! {"$match": base},
                doc! {"$match": {"$expr": all(conditions)}},
            ],
            "reading resources pointing at a page",
        )
        .await
}

async fn pulled_in(
    store: &DocumentStore,
    query: &SearchQuery,
    entries: &[(Document, ResourceEnvelope)],
) -> Result<Vec<ResourceEnvelope>, Error> {
    let mut seen: HashSet<String> = entries
        .iter()
        .map(|(_, envelope)| reference_of(envelope))
        .collect();
    let mut included: Vec<ResourceEnvelope> = Vec::new();
    let mut frontier: Vec<(Document, ResourceEnvelope)> = entries.to_vec();
    let mut round = 0;
    while !frontier.is_empty() && round < ROUNDS {
        let mut found: Vec<(Document, ResourceEnvelope)> = Vec::new();
        for rule in query.includes.iter().filter(|rule| round == 0 || rule.iterate) {
            let reached = match rule.direction {
                IncludeDirection::Forward => {
                    let mut texts = Vec::new();
                    for (held, envelope) in &frontier {
                        if !rule.covers(envelope.resource_type()) {
                            continue;
                        }
                        texts.extend(linked(rule, held, envelope)?);
                    }
                    by_reference(store, query, &texts, rule.target).await?
                }
                IncludeDirection::Reverse => {
                    let texts: Vec<String> = frontier
                        .iter()
                        .filter(|(_, envelope)| {
                            rule.target.is_none_or(|kind| kind == envelope.resource_type())
                        })
                        .map(|(_, envelope)| reference_of(envelope))
                        .collect();
                    pointing_at(store, query, rule, &texts).await?
                }
            };
            for held in reached {
                let envelope = envelope_of(&held)?;
                if seen.insert(reference_of(&envelope)) {
                    found.push((held, envelope));
                }
            }
        }
        included.extend(found.iter().map(|(_, envelope)| envelope.clone()));
        frontier = found;
        round += 1;
    }
    Ok(included)
}

pub async fn run(store: &DocumentStore, query: &SearchQuery) -> Result<SearchPage, Error> {
    let query = query.simplified();
    let key = PlanKey::of(&query);
    let plan = store.cache().chosen(&key, proposed(&query));
    let mut compiler = Compiler::new();
    let mut stages: Vec<Document> = Vec::new();
    let conditions = selection(store, &query, &mut compiler, &mut stages).await?;
    let base = base_of(&query, &plan);

    let mut selected = vec![doc! {"$match": base}];
    selected.extend(stages);
    selected.push(doc! {"$match": {"$expr": all(conditions)}});

    let found = match query.count {
        0 => Vec::new(),
        count => {
            let mut listing = selected.clone();
            ordering(&query, &mut listing);
            if query.offset > 0 {
                listing.push(doc! {"$skip": query.offset.min(i64::MAX as usize) as i64});
            }
            if count < usize::MAX {
                listing.push(doc! {"$limit": count.min(i64::MAX as usize) as i64});
            }
            store.listed(listing, "running a search").await?
        }
    };
    let entries: Vec<(Document, ResourceEnvelope)> = found
        .into_iter()
        .map(|held| envelope_of(&held).map(|envelope| (held, envelope)))
        .collect::<Result<Vec<(Document, ResourceEnvelope)>, Error>>()?;

    let drawn = examined(store, &query, &plan).await?;
    let total = match query.total {
        TotalMode::None => None,
        TotalMode::Accurate => {
            let mut counting = selected;
            counting.push(doc! {"$count": "total"});
            let counted = store.listed(counting, "counting matches").await?;
            Some(
                counted
                    .first()
                    .and_then(|held| held.get_i32("total").ok())
                    .unwrap_or_default()
                    .max(0) as usize,
            )
        }
        TotalMode::Estimate => Some(drawn as usize),
    };
    store.cache().observed(&key, drawn);
    let included = pulled_in(store, &query, &entries).await?;
    Ok(SearchPage {
        entries: entries.into_iter().map(|(_, envelope)| envelope).collect(),
        included,
        total,
        offset: query.offset,
    })
}

fn pull_of(param: &str) -> Document {
    let mut pull = Document::new();
    for array in ARRAYS {
        pull.insert(array, doc! {"param": param});
    }
    pull.insert("sort", doc! {"param": param});
    pull
}

fn push_of(rows: &Rows) -> Document {
    let entries = entries_of(rows);
    let mut push = Document::new();
    for (name, value) in entries {
        if let Bson::Array(items) = value {
            if !items.is_empty() {
                push.insert(name, doc! {"$each": items});
            }
        }
    }
    push
}

async fn rewrite(
    store: &DocumentStore,
    filter: Document,
    param: &str,
    rows: Option<&Rows>,
) -> Result<(), Error> {
    let mut work = store.writing().await?;
    let session = work.session()?;
    store
        .resources()
        .update_many(filter.clone(), doc! {"$pull": pull_of(param)})
        .session(&mut *session)
        .await
        .map_err(|error| faulted("clearing an index entry", error))?;
    if let Some(rows) = rows {
        let push = push_of(rows);
        if !push.is_empty() {
            let session = work.session()?;
            store
                .resources()
                .update_many(filter, doc! {"$push": push})
                .session(&mut *session)
                .await
                .map_err(|error| faulted("indexing values", error))?;
        }
    }
    work.done().await
}

pub async fn drop_index(store: &DocumentStore, url: &str) -> Result<(), Error> {
    rewrite(store, Document::new(), url, None).await
}

fn reported(spec: &ParameterSpec, rows: &Rows, report: &mut IndexReport) {
    let values = declared(rows);
    if values > 0 {
        report.indexed += 1;
        report.values += values;
        report.overflow += overflowed(rows);
    }
    let _ = spec;
}

pub async fn reindex(
    store: &DocumentStore,
    specs: &[ParameterSpec],
) -> Result<Vec<IndexReport>, Error> {
    let mut reports = Vec::new();
    for spec in specs {
        drop_index(store, &spec.url).await?;
        let mut report = IndexReport::empty(&spec.url);
        let mut filter = live();
        filter.insert("resource_type", doc! {"$in": names_of(&spec.base)});
        let found = store
            .listed(
                vec![doc! {"$match": filter}, doc! {"$sort": {"sequence": 1}}],
                "reading resources to index",
            )
            .await?;
        for held in &found {
            let envelope = envelope_of(held)?;
            let body = body_of(&envelope)?;
            match parses(spec, &body) {
                Err(reason) => report.failures.push(IndexFailure {
                    resource: reference_of(&envelope),
                    reason,
                }),
                Ok(()) => {
                    let rows = rows_of(&envelope, &body, &[Arc::clone(&spec.def)]);
                    reported(spec, &rows, &mut report);
                    let mut one = live();
                    one.insert("resource_id", envelope.id().as_str());
                    rewrite(store, one, &spec.url, Some(&rows)).await?;
                }
            }
        }
        report.backfilled = true;
        store.remember(spec.clone(), report.clone());
        reports.push(report);
    }
    Ok(reports)
}

pub async fn reindex_resource(
    store: &DocumentStore,
    specs: &[ParameterSpec],
    id: &fhir_core::ResourceId,
) -> Result<Vec<IndexReport>, Error> {
    let held = store
        .perhaps(
            doc! {"resource_id": id.as_str(), "is_current": true},
            "reading a resource to index",
        )
        .await?
        .ok_or(Error::NotFound)?;
    let envelope = envelope_of(&held)?;
    let body = body_of(&envelope)?;
    let mut reports = Vec::new();
    for spec in specs {
        let mut report = IndexReport::empty(&spec.url);
        report.backfilled = store
            .reported(&spec.url)
            .map(|held| held.backfilled)
            .unwrap_or_default();
        if !spec.base.contains(&envelope.resource_type()) {
            reports.push(report);
            continue;
        }
        let mut one = Document::new();
        one.insert("resource_id", envelope.id().as_str());
        one.insert("is_current", true);
        if envelope.is_deleted() {
            rewrite(store, one, &spec.url, None).await?;
            reports.push(report);
            continue;
        }
        match parses(spec, &body) {
            Err(reason) => {
                rewrite(store, one, &spec.url, None).await?;
                report.failures.push(IndexFailure {
                    resource: reference_of(&envelope),
                    reason,
                });
            }
            Ok(()) => {
                let rows = rows_of(&envelope, &body, &[Arc::clone(&spec.def)]);
                let values = declared(&rows);
                if values > 0 {
                    report.indexed = 1;
                    report.values = values;
                    report.overflow = overflowed(&rows);
                }
                rewrite(store, one, &spec.url, Some(&rows)).await?;
            }
        }
        reports.push(report);
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::search::lookup;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a known type")
    }

    fn filter(resource_type: &str, name: &str, raw: &str) -> Filter {
        let def = lookup(Some(kind(resource_type)), name).expect("a built-in parameter");
        let values = vec![def.value(raw).expect("a valid value")];
        Filter::new(name, def.target.clone(), values)
    }

    #[test]
    fn a_query_selecting_on_an_indexed_parameter_draws_from_that_index() {
        let query = SearchQuery {
            filters: vec![filter("Observation", "code", "code-1")],
            ..SearchQuery::of_type(kind("Observation"))
        };
        assert!(proposed(&query).is_indexed());
        let base = base_of(&query, &proposed(&query));
        assert!(base.contains_key("token"), "{base:?}");
    }

    #[test]
    fn a_query_selecting_on_nothing_reads_every_candidate() {
        let query = SearchQuery::of_type(kind("Patient"));
        assert_eq!(proposed(&query), Plan::Scan);
        let base = base_of(&query, &Plan::Scan);
        assert!(!base.contains_key("token"), "{base:?}");
        assert_eq!(base.get_bool("is_current"), Ok(true));
    }

    #[test]
    fn ordering_reads_the_key_the_resource_was_indexed_under() {
        let query = SearchQuery {
            sort: vec![fhir_store::SortKey {
                name: "name".to_owned(),
                target: lookup(Some(kind("Patient")), "name").unwrap().target.clone(),
                direction: SortDirection::Descending,
            }],
            ..SearchQuery::of_type(kind("Patient"))
        };
        let mut pipeline = Vec::new();
        ordering(&query, &mut pipeline);
        assert_eq!(pipeline.len(), 2);
        assert!(pipeline[0].contains_key("$addFields"));
        let order = pipeline[1].get_document("$sort").expect("an order");
        assert_eq!(order.get_i32("order0"), Ok(-1));
        assert_eq!(order.get_i32("resource_id"), Ok(1));
    }

    #[test]
    fn clearing_a_parameter_reaches_every_array_it_was_written_to() {
        let pull = pull_of("urn:p:band");
        for array in ARRAYS {
            assert!(pull.contains_key(array), "{array}");
        }
        assert!(pull.contains_key("sort"));
    }
}
