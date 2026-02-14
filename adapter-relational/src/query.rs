use crate::compile::{Bind, Compiler};
use crate::extract::{rows_of, MAIN};
use crate::row::{columns, envelope_of, COLUMNS};
use crate::store::{faulted, RelationalStore};
use fhir_core::search::{
    select, Criterion, Filter, Include, IncludeDirection, Modifier, ParameterSpec, SearchValue,
    Target,
};
use fhir_core::{Error, ResourceEnvelope, ResourceType};
use fhir_store::{
    IndexFailure, IndexReport, Plan, PlanKey, SearchPage, SearchQuery, SortDirection, TotalMode,
};
use serde_json::Value;
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::query::Query;
use sqlx::{Postgres, Row};
use std::collections::HashSet;

const ROUNDS: usize = 5;
const INDEX_TABLES: [&str; 7] = [
    "index_token",
    "index_text",
    "index_number",
    "index_date",
    "index_quantity",
    "index_reference",
    "index_uri",
];

pub(crate) fn apply<'a>(statement: &'a str, binds: &[Bind]) -> Query<'a, Postgres, PgArguments> {
    let mut prepared = sqlx::query(statement);
    for bind in binds {
        prepared = match bind {
            Bind::Text(text) => prepared.bind(text.clone()),
            Bind::Nullable(text) => prepared.bind(text.clone()),
            Bind::Int(number) => prepared.bind(*number),
            Bind::Big(number) => prepared.bind(*number),
            Bind::Real(number) => prepared.bind(*number),
            Bind::Texts(values) => prepared.bind(values.clone()),
        };
    }
    prepared
}

fn body_of(envelope: &ResourceEnvelope) -> Result<Value, Error> {
    serde_json::from_slice(envelope.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

fn reference_of(envelope: &ResourceEnvelope) -> String {
    format!(
        "{}/{}",
        envelope.resource_type().as_str(),
        envelope.id().as_str()
    )
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

fn linked(rule: &Include, body: &Value) -> Vec<String> {
    let mut found = Vec::new();
    match rule.is_wildcard() {
        true => every_pointer(body, &mut found),
        false => {
            for path in &rule.paths {
                for element in select(body, path) {
                    pointers(element, &mut found);
                }
            }
        }
    }
    found
}

fn normalized(text: &str) -> String {
    let mut parts = text.rsplit('/');
    let id = parts.next().unwrap_or_default();
    match parts.next() {
        Some(kind) => format!("{kind}/{id}"),
        None => id.to_owned(),
    }
}

fn logical(text: &str) -> String {
    text.rsplit('/').next().unwrap_or_default().to_owned()
}

async fn code_set(store: &RelationalStore, url: &str) -> Result<Vec<SearchValue>, Error> {
    let statement = format!(
        "select {COLUMNS} from {} where is_current and not is_deleted
         and resource_type = 'ValueSet'",
        store.table("resource")
    );
    let rows = store.listed(&statement, &[], "reading a code set").await?;
    for row in &rows {
        let envelope = envelope_of(row)?;
        let body = body_of(&envelope)?;
        if body.get("url").and_then(Value::as_str) == Some(url) {
            return Ok(fhir_core::search::code_set(&body));
        }
    }
    Err(Error::InvalidParameter(format!("code set {url:?} is unknown")))
}

async fn expanded(store: &RelationalStore, filter: &Filter) -> Result<Filter, Error> {
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

async fn members(store: &RelationalStore, id: &fhir_core::ResourceId) -> Result<Vec<String>, Error> {
    let statement = format!(
        "select {COLUMNS} from {} where resource_id = $1 and is_current and not is_deleted",
        store.table("resource")
    );
    let binds = [Bind::Text(id.as_str().to_owned())];
    let row = store.perhaps(&statement, &binds, "reading list membership").await?;
    let Some(row) = row else { return Ok(Vec::new()) };
    let body = body_of(&envelope_of(&row)?)?;
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

struct Selection {
    conditions: Vec<String>,
    binds: Vec<Bind>,
}

async fn selection(
    store: &RelationalStore,
    query: &SearchQuery,
    compiler: &mut Compiler<'_>,
) -> Result<Vec<String>, Error> {
    let mut conditions = vec!["r.is_current".to_owned(), "not r.is_deleted".to_owned()];
    if !query.types.is_empty() {
        let names = query.types.iter().map(|kind| kind.as_str().to_owned()).collect();
        let bound = compiler.place(Bind::Texts(names));
        conditions.push(format!("r.resource_type = any({bound})"));
    }
    if let Some(list) = &query.list {
        let found = members(store, list).await?;
        let bound = compiler.place(Bind::Texts(found));
        conditions.push(format!(
            "r.resource_type || '/' || r.resource_id = any({bound})"
        ));
    }
    if let Some(grant) = &query.grant {
        conditions.push(compiler.grant(grant, "r")?);
    }
    if let Some(compartment) = &query.compartment {
        conditions.push(compiler.compartment(compartment, "r"));
    }
    for filter in &query.filters {
        let resolved = expanded(store, filter).await?;
        conditions.push(compiler.filter(&resolved, "r")?);
    }
    for chain in &query.chains {
        let criterion = Criterion::Linked(chain.clone());
        conditions.push(compiler.criterion(&criterion, "r", query.grant.as_ref())?);
    }
    Ok(conditions)
}

fn drive(store: &RelationalStore, filter: &Filter, compiler: &mut Compiler<'_>) -> Option<String> {
    let param = filter.index.clone().unwrap_or_else(|| filter.name.clone());
    let table = crate::compile::table_for(filter)?;
    let key = compiler.place(Bind::Text(param));
    let slot = compiler.place(Bind::Text(MAIN.to_owned()));
    let mut parts = Vec::new();
    for value in &filter.values {
        parts.push(compiler.value_condition(value, "d"));
    }
    Some(format!(
        "r.surrogate_id in (select d.surrogate_id from {} d where d.param = {key} \
         and d.slot = {slot} and ({}))",
        store.table(table),
        parts.join(" or ")
    ))
}

async fn examined(
    store: &RelationalStore,
    query: &SearchQuery,
    plan: &Plan,
) -> Result<u64, Error> {
    let mut compiler = Compiler::new(store);
    let statement = match plan {
        Plan::Indexed { .. } => match driving(query).and_then(|filter| drive(store, filter, &mut compiler)) {
            Some(clause) => format!(
                "select count(*) as total from {} r where r.is_current and not r.is_deleted and {clause}",
                store.table("resource")
            ),
            None => return Ok(0),
        },
        Plan::Scan => {
            let mut conditions = vec!["r.is_current".to_owned(), "not r.is_deleted".to_owned()];
            if !query.types.is_empty() {
                let names = query.types.iter().map(|kind| kind.as_str().to_owned()).collect();
                let bound = compiler.place(Bind::Texts(names));
                conditions.push(format!("r.resource_type = any({bound})"));
            }
            format!(
                "select count(*) as total from {} r where {}",
                store.table("resource"),
                conditions.join(" and ")
            )
        }
    };
    let binds = compiler.into_binds();
    let total: i64 = store
        .only(&statement, &binds, "measuring a plan")
        .await?
        .try_get("total")
        .map_err(|error| faulted("measuring a plan", error))?;
    Ok(total.max(0) as u64)
}

async fn estimated(store: &RelationalStore, statement: &str, binds: &[Bind]) -> Result<usize, Error> {
    let explained = format!("explain {statement}");
    let rows = store.listed(&explained, binds, "estimating a total").await?;
    for row in &rows {
        let line: String = row.try_get(0).unwrap_or_default();
        if let Some(rest) = line.split("rows=").nth(1) {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(found) = digits.parse::<usize>() {
                return Ok(found);
            }
        }
    }
    Ok(0)
}

async fn fetch(
    store: &RelationalStore,
    statement: &str,
    binds: &[Bind],
) -> Result<Vec<ResourceEnvelope>, Error> {
    let rows: Vec<PgRow> = store.listed(statement, binds, "running a search").await?;
    rows.iter().map(envelope_of).collect()
}

async fn by_reference(
    store: &RelationalStore,
    query: &SearchQuery,
    texts: &[String],
    target: Option<ResourceType>,
) -> Result<Vec<ResourceEnvelope>, Error> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let mut compiler = Compiler::new(store);
    let full: Vec<String> = texts.iter().map(|text| normalized(text)).collect();
    let bare: Vec<String> = texts.iter().map(|text| logical(text)).collect();
    let full = compiler.place(Bind::Texts(full));
    let bare = compiler.place(Bind::Texts(bare));
    let mut conditions = vec![
        "r.is_current".to_owned(),
        "not r.is_deleted".to_owned(),
        format!("(r.resource_type || '/' || r.resource_id = any({full}) or r.resource_id = any({bare}))"),
    ];
    if let Some(kind) = target {
        let bound = compiler.place(Bind::Text(kind.as_str().to_owned()));
        conditions.push(format!("r.resource_type = {bound}"));
    }
    if let Some(grant) = &query.grant {
        conditions.push(compiler.grant(grant, "r")?);
    }
    let statement = format!(
        "select {} from {} r where {}",
        columns("r"),
        store.table("resource"),
        conditions.join(" and ")
    );
    let binds = compiler.into_binds();
    fetch(store, &statement, &binds).await
}

async fn pointing_at(
    store: &RelationalStore,
    query: &SearchQuery,
    rule: &Include,
    texts: &[String],
) -> Result<Vec<ResourceEnvelope>, Error> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let mut compiler = Compiler::new(store);
    let full: Vec<String> = texts.iter().map(|text| normalized(text)).collect();
    let bare: Vec<String> = texts.iter().map(|text| logical(text)).collect();
    let full = compiler.place(Bind::Texts(full));
    let bare = compiler.place(Bind::Texts(bare));
    let name = compiler.place(Bind::Text(rule.name.clone()));
    let slot = compiler.place(Bind::Text(MAIN.to_owned()));
    let mut conditions = vec![
        "r.is_current".to_owned(),
        "not r.is_deleted".to_owned(),
        format!(
            "exists (select 1 from {} k where k.surrogate_id = r.surrogate_id \
             and k.param = {name} and k.slot = {slot} \
             and (k.ref_full = any({full}) or k.ref_id = any({bare})))",
            store.table("index_reference")
        ),
    ];
    if let Some(source) = rule.source {
        let bound = compiler.place(Bind::Text(source.as_str().to_owned()));
        conditions.push(format!("r.resource_type = {bound}"));
    }
    if let Some(grant) = &query.grant {
        conditions.push(compiler.grant(grant, "r")?);
    }
    let statement = format!(
        "select {} from {} r where {}",
        columns("r"),
        store.table("resource"),
        conditions.join(" and ")
    );
    let binds = compiler.into_binds();
    fetch(store, &statement, &binds).await
}

async fn pulled_in(
    store: &RelationalStore,
    query: &SearchQuery,
    entries: &[ResourceEnvelope],
) -> Result<Vec<ResourceEnvelope>, Error> {
    let mut seen: HashSet<String> = entries.iter().map(reference_of).collect();
    let mut included: Vec<ResourceEnvelope> = Vec::new();
    let mut frontier: Vec<ResourceEnvelope> = entries.to_vec();
    let mut round = 0;
    while !frontier.is_empty() && round < ROUNDS {
        let mut found: Vec<ResourceEnvelope> = Vec::new();
        for rule in query.includes.iter().filter(|rule| round == 0 || rule.iterate) {
            let reached = match rule.direction {
                IncludeDirection::Forward => {
                    let mut texts = Vec::new();
                    for envelope in &frontier {
                        if !rule.covers(envelope.resource_type()) {
                            continue;
                        }
                        texts.extend(linked(rule, &body_of(envelope)?));
                    }
                    by_reference(store, query, &texts, rule.target).await?
                }
                IncludeDirection::Reverse => {
                    let texts: Vec<String> = frontier
                        .iter()
                        .filter(|envelope| {
                            rule.target.is_none_or(|kind| kind == envelope.resource_type())
                        })
                        .map(reference_of)
                        .collect();
                    pointing_at(store, query, rule, &texts).await?
                }
            };
            for envelope in reached {
                if seen.insert(reference_of(&envelope)) {
                    found.push(envelope);
                }
            }
        }
        included.extend(found.iter().cloned());
        frontier = found;
        round += 1;
    }
    Ok(included)
}

pub async fn run(store: &RelationalStore, query: &SearchQuery) -> Result<SearchPage, Error> {
    let query = query.simplified();
    let key = PlanKey::of(&query);
    let plan = store.cache().chosen(&key, proposed(&query));
    let mut compiler = Compiler::new(store);
    let mut conditions = selection(store, &query, &mut compiler).await?;
    if plan.is_indexed() {
        if let Some(clause) = driving(&query).and_then(|filter| drive(store, filter, &mut compiler)) {
            conditions.push(clause);
        }
    }
    let selected = Selection {
        conditions,
        binds: compiler.binds().to_vec(),
    };
    let table = store.table("resource");
    let where_clause = selected.conditions.join(" and ");

    let mut ordering: Vec<String> = Vec::new();
    let mut order_binds = selected.binds.clone();
    for sort in &query.sort {
        order_binds.push(Bind::Text(sort.name.clone()));
        let place = order_binds.len();
        let direction = match sort.direction {
            SortDirection::Ascending => "asc",
            SortDirection::Descending => "desc",
        };
        ordering.push(format!(
            "(select s.sort_text from {} s where s.surrogate_id = r.surrogate_id \
             and s.param = ${place}) {direction}",
            store.table("index_sort")
        ));
    }
    ordering.push("r.resource_id asc".to_owned());

    let mut page_binds = order_binds.clone();
    page_binds.push(Bind::Big(query.count.min(i64::MAX as usize) as i64));
    let limit = page_binds.len();
    page_binds.push(Bind::Big(query.offset.min(i64::MAX as usize) as i64));
    let offset = page_binds.len();
    let listing = format!(
        "select {} from {table} r where {where_clause} order by {} limit ${limit} offset ${offset}",
        columns("r"),
        ordering.join(", ")
    );
    let entries = fetch(store, &listing, &page_binds).await?;

    let counting = format!("select count(*) as total from {table} r where {where_clause}");
    let total = match query.total {
        TotalMode::None => None,
        TotalMode::Accurate => {
            let row = store
                .only(&counting, &selected.binds, "counting matches")
                .await?;
            let found: i64 = row
                .try_get("total")
                .map_err(|error| faulted("counting matches", error))?;
            Some(found.max(0) as usize)
        }
        TotalMode::Estimate => {
            let probe = format!("select 1 from {table} r where {where_clause}");
            Some(estimated(store, &probe, &selected.binds).await?)
        }
    };

    store.cache().observed(&key, examined(store, &query, &plan).await?);
    let included = pulled_in(store, &query, &entries).await?;
    Ok(SearchPage {
        entries,
        included,
        total,
        offset: query.offset,
    })
}

pub async fn drop_index(store: &RelationalStore, url: &str) -> Result<(), Error> {
    for table in INDEX_TABLES {
        let statement = format!("delete from {} where param = $1", store.table(table));
        let binds = [Bind::Text(url.to_owned())];
        store.ran(&statement, &binds, "dropping an index").await?;
    }
    Ok(())
}

pub async fn reindex(
    store: &RelationalStore,
    specs: &[ParameterSpec],
) -> Result<Vec<IndexReport>, Error> {
    let mut reports = Vec::new();
    for spec in specs {
        drop_index(store, &spec.url).await?;
        let mut report = IndexReport::empty(&spec.url);
        let names: Vec<String> = spec.base.iter().map(|kind| kind.as_str().to_owned()).collect();
        let statement = format!(
            "select {COLUMNS} from {} where is_current and not is_deleted
             and resource_type = any($1) order by surrogate_id",
            store.table("resource")
        );
        let binds = [Bind::Texts(names)];
        let rows = store
            .listed(&statement, &binds, "reading resources to index")
            .await?;
        for row in &rows {
            let envelope = envelope_of(row)?;
            let surrogate: i64 = row
                .try_get("surrogate_id")
                .map_err(|error| faulted("reading resources to index", error))?;
            let body = body_of(&envelope)?;
            match checked(spec, &body) {
                Err(reason) => report.failures.push(IndexFailure {
                    resource: reference_of(&envelope),
                    reason,
                }),
                Ok(()) => {
                    let rows = rows_of(&envelope, &body, &[std::sync::Arc::clone(&spec.def)]);
                    let values = rows
                        .tokens
                        .iter()
                        .filter(|row| row.slot == MAIN)
                        .count()
                        + rows.texts.iter().filter(|row| row.slot == MAIN).count()
                        + rows.numbers.len()
                        + rows.dates.len()
                        + rows.quantities.len()
                        + rows.references.len()
                        + rows.uris.len();
                    if values > 0 {
                        report.indexed += 1;
                        report.values += values;
                        report.overflow += rows
                            .tokens
                            .iter()
                            .filter(|row| row.code_tail.is_some())
                            .count();
                    }
                    let mut work = store.work().await?;
                    store.index(work.conn()?, surrogate, &rows).await?;
                    work.done().await?;
                }
            }
        }
        report.backfilled = true;
        store.record(&report).await?;
        store.remember(spec.clone(), report.clone());
        reports.push(report);
    }
    Ok(reports)
}

fn checked(spec: &ParameterSpec, body: &Value) -> Result<(), String> {
    for path in spec.def.paths() {
        for element in select(body, &path) {
            let mut found = Vec::new();
            flatten(element, &mut found);
            for text in found {
                SearchValue::parse(spec.def.value_type, &text).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn flatten(element: &Value, out: &mut Vec<String>) {
    match element {
        Value::String(text) => out.push(text.clone()),
        Value::Number(number) => out.push(number.to_string()),
        Value::Bool(flag) => out.push(flag.to_string()),
        Value::Array(items) => items.iter().for_each(|item| flatten(item, out)),
        Value::Object(_) | Value::Null => {}
    }
}

pub async fn reindex_resource(
    store: &RelationalStore,
    specs: &[ParameterSpec],
    id: &fhir_core::ResourceId,
) -> Result<Vec<IndexReport>, Error> {
    let statement = format!(
        "select {COLUMNS} from {} where resource_id = $1 and is_current",
        store.table("resource")
    );
    let binds = [Bind::Text(id.as_str().to_owned())];
    let row = store
        .perhaps(&statement, &binds, "reading a resource to index")
        .await?
        .ok_or(Error::NotFound)?;
    let envelope = envelope_of(&row)?;
    let surrogate: i64 = row
        .try_get("surrogate_id")
        .map_err(|error| faulted("reading a resource to index", error))?;
    let body = body_of(&envelope)?;
    let mut reports = Vec::new();
    for spec in specs {
        let mut report = IndexReport::empty(&spec.url);
        report.backfilled = store
            .recorded(&spec.url)
            .await?
            .map(|held| held.backfilled)
            .unwrap_or_default();
        if !spec.base.contains(&envelope.resource_type()) {
            reports.push(report);
            continue;
        }
        for table in INDEX_TABLES {
            let statement = format!(
                "delete from {} where surrogate_id = $1 and param = $2",
                store.table(table)
            );
            let binds = [Bind::Big(surrogate), Bind::Text(spec.url.clone())];
            store
                .ran(&statement, &binds, "clearing an index entry")
                .await?;
        }
        if envelope.is_deleted() {
            reports.push(report);
            continue;
        }
        match checked(spec, &body) {
            Err(reason) => report.failures.push(IndexFailure {
                resource: reference_of(&envelope),
                reason,
            }),
            Ok(()) => {
                let rows = rows_of(&envelope, &body, &[std::sync::Arc::clone(&spec.def)]);
                let values = rows.tokens.iter().filter(|row| row.slot == MAIN).count()
                    + rows.texts.iter().filter(|row| row.slot == MAIN).count()
                    + rows.numbers.len()
                    + rows.dates.len()
                    + rows.quantities.len()
                    + rows.references.len()
                    + rows.uris.len();
                if values > 0 {
                    report.indexed = 1;
                    report.values = values;
                    report.overflow = rows
                        .tokens
                        .iter()
                        .filter(|row| row.code_tail.is_some())
                        .count();
                }
                let mut work = store.work().await?;
                store.index(work.conn()?, surrogate, &rows).await?;
                work.done().await?;
            }
        }
        reports.push(report);
    }
    Ok(reports)
}
