use async_trait::async_trait;
use fhir_core::search::{
    ChainDirection, Compartment, Criterion, Filter, Grant, Include, IncludeDirection, Modifier,
    Target, TokenSystem,
};
use fhir_core::search::{IndexKey, ParameterSpec, SearchValue};
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::{
    system_clock, Clock, HistoryOrder, HistoryPage, HistoryQuery, HistoryScope, IndexFailure,
    IndexReport, Plan, PlanCache, PlanKey, PlanStat, ResourceStore, SearchPage, SearchQuery,
    SortDirection, SortKey, StoreScope, TotalMode,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tokio::sync::{Mutex, OwnedMutexGuard};

type StoreMap = HashMap<ResourceId, Vec<ResourceEnvelope>>;

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

fn list_members(guard: &StoreMap, id: &ResourceId) -> Result<HashSet<String>, Error> {
    let Some(current) = guard.get(id).and_then(|versions| versions.last()) else {
        return Ok(HashSet::new());
    };
    if current.is_deleted() {
        return Ok(HashSet::new());
    }
    let body = body_of(current)?;
    Ok(fhir_core::search::select(&body, "entry.item.reference")
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect())
}

fn code_set(guard: &StoreMap, url: &str) -> Result<Vec<fhir_core::SearchValue>, Error> {
    for versions in guard.values() {
        let Some(current) = versions.last() else {
            continue;
        };
        if current.is_deleted() || current.resource_type().as_str() != "ValueSet" {
            continue;
        }
        let body = body_of(current)?;
        if body.get("url").and_then(Value::as_str) == Some(url) {
            return Ok(fhir_core::search::code_set(&body));
        }
    }
    Err(Error::InvalidParameter(format!(
        "code set {url:?} is unknown"
    )))
}

fn expanded(guard: &StoreMap, filter: &Filter) -> Result<Filter, Error> {
    let addresses = filter.code_sets();
    if addresses.is_empty() {
        return Ok(filter.clone());
    }
    let mut values = Vec::new();
    for address in addresses {
        values.extend(code_set(guard, &address)?);
    }
    Ok(filter.resolved(&values))
}

enum Resolved {
    Direct(Filter),
    Forward {
        target: Target,
        refs: HashSet<String>,
    },
    Reverse {
        refs: HashSet<String>,
    },
}

fn references(element: &Value) -> Vec<String> {
    fhir_core::search::pointers(element)
}

fn normalized(text: &str) -> String {
    let mut parts = text.rsplit('/');
    let id = parts.next().unwrap_or_default();
    match parts.next() {
        Some(kind) => format!("{kind}/{id}"),
        None => id.to_owned(),
    }
}

fn record(refs: &mut HashSet<String>, text: &str) {
    let full = normalized(text);
    if let Some((_, id)) = full.split_once('/') {
        refs.insert(id.to_owned());
    }
    refs.insert(full);
}

fn at(target: &Target, body: &Value) -> Vec<String> {
    match target {
        Target::Path(paths) => paths
            .iter()
            .flat_map(|path| fhir_core::search::select(body, path))
            .flat_map(references)
            .collect(),
        Target::Id | Target::LastUpdated | Target::Composite(_) => Vec::new(),
    }
}

fn resolve(
    guard: &StoreMap,
    criterion: &Criterion,
    grant: Option<&Grant>,
) -> Result<Resolved, Error> {
    let chain = match criterion {
        Criterion::Direct(filter) => return Ok(Resolved::Direct(expanded(guard, filter)?)),
        Criterion::Linked(chain) => chain,
    };
    let inner = resolve(guard, &chain.next, grant)?;
    let mut refs = HashSet::new();
    for versions in guard.values() {
        let Some(current) = versions.last() else {
            continue;
        };
        if current.is_deleted() {
            continue;
        }
        if !chain.types.is_empty() && !chain.types.contains(&current.resource_type()) {
            continue;
        }
        let body = body_of(current)?;
        if !admitted(grant, current, &body) {
            continue;
        }
        if !holds(&inner, current, &body) {
            continue;
        }
        match chain.direction {
            ChainDirection::Forward => record(&mut refs, &reference_of(current)),
            ChainDirection::Reverse => {
                for text in at(&chain.target, &body) {
                    record(&mut refs, &text);
                }
            }
        }
    }
    Ok(match chain.direction {
        ChainDirection::Forward => Resolved::Forward {
            target: chain.target.clone(),
            refs,
        },
        ChainDirection::Reverse => Resolved::Reverse { refs },
    })
}

fn holds(resolved: &Resolved, envelope: &ResourceEnvelope, body: &Value) -> bool {
    match resolved {
        Resolved::Direct(filter) => filter.matches(envelope.id(), envelope.last_updated(), body),
        Resolved::Forward { target, refs } => at(target, body)
            .iter()
            .any(|text| refs.contains(&normalized(text))),
        Resolved::Reverse { refs } => {
            refs.contains(&reference_of(envelope)) || refs.contains(envelope.id().as_str())
        }
    }
}

fn in_compartment(compartment: &Compartment, envelope: &ResourceEnvelope, body: &Value) -> bool {
    fhir_core::search::compartment::contains(compartment, envelope.resource_type(), body)
}

fn admitted(grant: Option<&Grant>, envelope: &ResourceEnvelope, body: &Value) -> bool {
    grant.is_none_or(|grant| grant.reaches(envelope, body))
}

const INCLUDE_ROUNDS: usize = 5;

fn every_reference(body: &Value, out: &mut Vec<String>) {
    match body {
        Value::Array(items) => items.iter().for_each(|item| every_reference(item, out)),
        Value::Object(map) => {
            if let Some(text) = map.get("reference").and_then(Value::as_str) {
                out.push(text.to_owned());
            }
            map.values().for_each(|nested| every_reference(nested, out));
        }
        Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

fn linked(rule: &Include, body: &Value) -> Vec<String> {
    if rule.is_wildcard() {
        let mut found = Vec::new();
        every_reference(body, &mut found);
        return found;
    }
    rule.paths
        .iter()
        .flat_map(|path| fhir_core::search::select(body, path))
        .flat_map(references)
        .collect()
}

fn stored<'a>(guard: &'a StoreMap, text: &str) -> Option<&'a ResourceEnvelope> {
    let full = normalized(text);
    let (kind, id) = match full.split_once('/') {
        Some((kind, id)) => (Some(kind), id),
        None => (None, full.as_str()),
    };
    let current = guard.get(&ResourceId::parse(id).ok()?)?.last()?;
    if current.is_deleted() || kind.is_some_and(|kind| kind != current.resource_type().as_str()) {
        return None;
    }
    Some(current)
}

fn pulled_in(
    guard: &StoreMap,
    entries: &[ResourceEnvelope],
    rules: &[Include],
    grant: Option<&Grant>,
) -> Result<Vec<ResourceEnvelope>, Error> {
    let mut seen: HashSet<String> = entries.iter().map(reference_of).collect();
    let mut included: Vec<ResourceEnvelope> = Vec::new();
    let mut frontier: Vec<ResourceEnvelope> = entries.to_vec();
    let mut round = 0;
    while !frontier.is_empty() && round < INCLUDE_ROUNDS {
        let mut found: Vec<ResourceEnvelope> = Vec::new();
        for rule in rules.iter().filter(|rule| round == 0 || rule.iterate) {
            match rule.direction {
                IncludeDirection::Forward => {
                    for envelope in &frontier {
                        if !rule.covers(envelope.resource_type()) {
                            continue;
                        }
                        let body = body_of(envelope)?;
                        for text in linked(rule, &body) {
                            let Some(target) = stored(guard, &text) else {
                                continue;
                            };
                            if rule
                                .target
                                .is_some_and(|kind| kind != target.resource_type())
                            {
                                continue;
                            }
                            if !admitted(grant, target, &body_of(target)?) {
                                continue;
                            }
                            if seen.insert(reference_of(target)) {
                                found.push(target.clone());
                            }
                        }
                    }
                }
                IncludeDirection::Reverse => {
                    let mut wanted = HashSet::new();
                    for envelope in &frontier {
                        if rule
                            .target
                            .is_none_or(|kind| kind == envelope.resource_type())
                        {
                            record(&mut wanted, &reference_of(envelope));
                        }
                    }
                    for versions in guard.values() {
                        let Some(current) = versions.last() else {
                            continue;
                        };
                        if current.is_deleted() || !rule.covers(current.resource_type()) {
                            continue;
                        }
                        let body = body_of(current)?;
                        if !admitted(grant, current, &body) {
                            continue;
                        }
                        let hit = linked(rule, &body)
                            .iter()
                            .any(|text| wanted.contains(&normalized(text)));
                        if hit && seen.insert(reference_of(current)) {
                            found.push(current.clone());
                        }
                    }
                }
            }
        }
        included.extend(found.iter().cloned());
        frontier = found;
        round += 1;
    }
    Ok(included)
}

fn order(matches: &mut [(ResourceEnvelope, Value)], keys: &[SortKey]) {
    matches.sort_by(|left, right| {
        for key in keys {
            let a = fhir_core::search::sort_value(
                &key.target,
                left.0.id(),
                left.0.last_updated(),
                &left.1,
            );
            let b = fhir_core::search::sort_value(
                &key.target,
                right.0.id(),
                right.0.last_updated(),
                &right.1,
            );
            let ordering = match key.direction {
                SortDirection::Ascending => a.cmp(&b),
                SortDirection::Descending => b.cmp(&a),
            };
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        left.0.id().as_str().cmp(right.0.id().as_str())
    });
}

#[derive(Clone)]
struct ParamIndex {
    entries: HashMap<ResourceId, Vec<Value>>,
    report: IndexReport,
}

type IndexMap = HashMap<String, ParamIndex>;

fn extracted(spec: &ParameterSpec, body: &Value) -> Result<Vec<Value>, String> {
    let mut found = Vec::new();
    for path in spec.def.paths() {
        for element in fhir_core::search::select(body, &path) {
            match element {
                Value::Array(items) => found.extend(items.iter().cloned()),
                Value::Null => {}
                other => found.push(other.clone()),
            }
        }
    }
    for element in &found {
        if let Some(text) = scalar_text(element) {
            SearchValue::parse(spec.def.value_type, &text).map_err(|error| error.to_string())?;
        }
    }
    Ok(found)
}

fn scalar_text(element: &Value) -> Option<String> {
    match element {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Array(_) | Value::Object(_) | Value::Null => None,
    }
}

fn indexed(indexes: &IndexMap, url: &str, id: &ResourceId) -> Vec<Value> {
    indexes
        .get(url)
        .and_then(|index| index.entries.get(id))
        .cloned()
        .unwrap_or_default()
}

fn recounted(index: &mut ParamIndex) {
    index.report.indexed = index.entries.len();
    index.report.values = index.entries.values().map(Vec::len).sum();
    index.report.overflow = index
        .entries
        .values()
        .map(|elements| overflowing(elements))
        .sum();
}
fn overflowing(elements: &[Value]) -> usize {
    elements
        .iter()
        .filter_map(scalar_text)
        .filter(|text| IndexKey::of(text).overflows())
        .count()
}

pub struct MemoryStore {
    inner: Arc<RwLock<StoreMap>>,
    clock: Clock,
    plans: Arc<PlanCache>,
    indexes: Arc<RwLock<IndexMap>>,
    gate: Arc<Mutex<()>>,
    scoped: bool,
}

impl MemoryStore {
    pub fn with_clock(clock: Clock) -> MemoryStore {
        MemoryStore {
            inner: Arc::new(RwLock::new(HashMap::new())),
            clock,
            plans: Arc::new(PlanCache::new()),
            indexes: Arc::new(RwLock::new(HashMap::new())),
            gate: Arc::new(Mutex::new(())),
            scoped: false,
        }
    }

    pub fn plans(&self) -> Vec<PlanStat> {
        self.plans.stats()
    }

    async fn hold(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        match self.scoped {
            true => None,
            false => Some(self.gate.lock().await),
        }
    }

    fn sharing(&self, scoped: bool) -> MemoryStore {
        MemoryStore {
            inner: Arc::clone(&self.inner),
            clock: Arc::clone(&self.clock),
            plans: Arc::clone(&self.plans),
            indexes: Arc::clone(&self.indexes),
            gate: Arc::clone(&self.gate),
            scoped,
        }
    }
}

fn proposed(query: &SearchQuery) -> Plan {
    match indexed_ids(query) {
        Some(_) => Plan::Indexed {
            parameter: "_id".to_owned(),
        },
        None => Plan::Scan,
    }
}

fn indexed_ids(query: &SearchQuery) -> Option<Vec<ResourceId>> {
    let filter = query
        .filters
        .iter()
        .find(|filter| filter.target == Target::Id && filter.modifier == Modifier::None)?;
    let mut ids = Vec::new();
    for value in &filter.values {
        let code = match value {
            fhir_core::SearchValue::Token(token) if token.system == TokenSystem::Any => {
                token.code.clone()?
            }
            _ => return None,
        };
        ids.push(ResourceId::parse(&code).ok()?);
    }
    (!ids.is_empty()).then_some(ids)
}

impl Default for MemoryStore {
    fn default() -> Self {
        MemoryStore::with_clock(system_clock())
    }
}

fn version_number(envelope: &ResourceEnvelope) -> u64 {
    envelope
        .version_id()
        .as_str()
        .parse::<u64>()
        .unwrap_or_default()
}

fn next_version(current: &VersionId) -> Result<VersionId, Error> {
    let number = current.as_str().parse::<u64>().map_err(|_| {
        Error::Internal(format!("non-numeric stored version {:?}", current.as_str()))
    })?;
    let next = number
        .checked_add(1)
        .ok_or_else(|| Error::Internal("version overflow".to_owned()))?;
    VersionId::parse(&next.to_string())
}

fn repeated(envelope: &ResourceEnvelope) -> Error {
    Error::Duplicate(format!(
        "version {} of {:?} is restored twice with a different body",
        envelope.version_id().as_str(),
        envelope.id().as_str()
    ))
}

#[async_trait]
impl ResourceStore for MemoryStore {
    async fn create(&self, envelope: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
        let _hold = self.hold().await;
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        if guard.contains_key(envelope.id()) {
            return Err(Error::Duplicate(format!(
                "id {:?} already exists",
                envelope.id().as_str()
            )));
        }
        let first: VersionId = "1".parse()?;
        let stored = envelope.stored_with(first, (self.clock)())?;
        guard.insert(envelope.id().clone(), vec![stored.clone()]);
        Ok(stored)
    }

    async fn read(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        match guard.get(id).and_then(|versions| versions.last()) {
            Some(current) => Ok(current.clone()),
            None => Err(Error::NotFound),
        }
    }

    async fn vread(&self, id: &ResourceId, version: &VersionId) -> Result<ResourceEnvelope, Error> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        match guard.get(id) {
            Some(versions) => versions
                .iter()
                .find(|stored| stored.version_id() == version)
                .cloned()
                .ok_or(Error::NotFound),
            None => Err(Error::NotFound),
        }
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, Error> {
        let query = &query.simplified();
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let indexes = self
            .indexes
            .read()
            .map_err(|_| Error::Internal("index lock poisoned".to_owned()))?;
        let key = PlanKey::of(query);
        let plan = self.plans.chosen(&key, proposed(query));
        let members = match &query.list {
            Some(id) => Some(list_members(&guard, id)?),
            None => None,
        };
        let filters = query
            .filters
            .iter()
            .map(|filter| expanded(&guard, filter))
            .collect::<Result<Vec<Filter>, Error>>()?;
        let grant = query.grant.as_ref();
        let chains = query
            .chains
            .iter()
            .map(|chain| resolve(&guard, &Criterion::Linked(chain.clone()), grant))
            .collect::<Result<Vec<Resolved>, Error>>()?;
        let candidates: Vec<&Vec<ResourceEnvelope>> = match &plan {
            Plan::Indexed { .. } => indexed_ids(query)
                .unwrap_or_default()
                .iter()
                .filter_map(|id| guard.get(id))
                .collect(),
            Plan::Scan => guard.values().collect(),
        };
        let mut examined: u64 = 0;
        let mut matches: Vec<(ResourceEnvelope, Value)> = Vec::new();
        for versions in candidates {
            examined += 1;
            let Some(current) = versions.last() else {
                continue;
            };
            if current.is_deleted() {
                continue;
            }
            if !query.types.is_empty() && !query.types.contains(&current.resource_type()) {
                continue;
            }
            if let Some(members) = &members {
                if !members.contains(&reference_of(current)) {
                    continue;
                }
            }
            let body = body_of(current)?;
            if !admitted(grant, current, &body) {
                continue;
            }
            if let Some(compartment) = &query.compartment {
                if !in_compartment(compartment, current, &body) {
                    continue;
                }
            }
            let kept = filters.iter().all(|filter| match &filter.index {
                Some(url) => filter.matches_indexed(&indexed(&indexes, url, current.id())),
                None => filter.matches(current.id(), current.last_updated(), &body),
            }) && chains.iter().all(|chain| holds(chain, current, &body));
            if kept {
                matches.push((current.clone(), body));
            }
        }
        self.plans.observed(&key, examined);
        order(&mut matches, &query.sort);
        let total = match query.total {
            TotalMode::None => None,
            TotalMode::Accurate | TotalMode::Estimate => Some(matches.len()),
        };
        let entries: Vec<ResourceEnvelope> = matches
            .into_iter()
            .skip(query.offset)
            .take(query.count)
            .map(|(envelope, _)| envelope)
            .collect();
        let included = pulled_in(&guard, &entries, &query.includes, grant)?;
        Ok(SearchPage {
            entries,
            included,
            total,
            offset: query.offset,
        })
    }

    async fn index_parameter(&self, spec: &ParameterSpec) -> Result<IndexReport, Error> {
        let mut indexes = self
            .indexes
            .write()
            .map_err(|_| Error::Internal("index lock poisoned".to_owned()))?;
        let report = IndexReport::empty(&spec.url);
        indexes.insert(
            spec.url.clone(),
            ParamIndex {
                entries: HashMap::new(),
                report: report.clone(),
            },
        );
        Ok(report)
    }

    async fn drop_parameter(&self, url: &str) -> Result<(), Error> {
        let mut indexes = self
            .indexes
            .write()
            .map_err(|_| Error::Internal("index lock poisoned".to_owned()))?;
        indexes.remove(url);
        Ok(())
    }

    async fn reindex(&self, specs: &[ParameterSpec]) -> Result<Vec<IndexReport>, Error> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let mut indexes = self
            .indexes
            .write()
            .map_err(|_| Error::Internal("index lock poisoned".to_owned()))?;
        let mut reports = Vec::new();
        for spec in specs {
            let mut index = ParamIndex {
                entries: HashMap::new(),
                report: IndexReport::empty(&spec.url),
            };
            for versions in guard.values() {
                let Some(current) = versions.last() else {
                    continue;
                };
                if current.is_deleted() || !spec.base.contains(&current.resource_type()) {
                    continue;
                }
                let body = body_of(current)?;
                match extracted(spec, &body) {
                    Ok(elements) if elements.is_empty() => {}
                    Ok(elements) => {
                        index.report.indexed += 1;
                        index.report.values += elements.len();
                        index.report.overflow += overflowing(&elements);
                        index.entries.insert(current.id().clone(), elements);
                    }
                    Err(reason) => index.report.failures.push(IndexFailure {
                        resource: reference_of(current),
                        reason,
                    }),
                }
            }
            index.report.backfilled = true;
            reports.push(index.report.clone());
            indexes.insert(spec.url.clone(), index);
        }
        Ok(reports)
    }

    async fn reindex_resource(
        &self,
        specs: &[ParameterSpec],
        id: &ResourceId,
    ) -> Result<Vec<IndexReport>, Error> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let current = guard
            .get(id)
            .and_then(|versions| versions.last())
            .ok_or(Error::NotFound)?;
        let body = body_of(current)?;
        let mut indexes = self
            .indexes
            .write()
            .map_err(|_| Error::Internal("index lock poisoned".to_owned()))?;
        let mut reports = Vec::new();
        for spec in specs {
            let mut report = IndexReport::empty(&spec.url);
            let Some(index) = indexes.get_mut(&spec.url) else {
                reports.push(report);
                continue;
            };
            report.backfilled = index.report.backfilled;
            if !spec.base.contains(&current.resource_type()) {
                reports.push(report);
                continue;
            }
            index.entries.remove(current.id());
            if !current.is_deleted() {
                match extracted(spec, &body) {
                    Ok(elements) if elements.is_empty() => {}
                    Ok(elements) => {
                        report.indexed = 1;
                        report.values = elements.len();
                        report.overflow = overflowing(&elements);
                        index.entries.insert(current.id().clone(), elements);
                    }
                    Err(reason) => report.failures.push(IndexFailure {
                        resource: reference_of(current),
                        reason,
                    }),
                }
            }
            recounted(index);
            reports.push(report);
        }
        Ok(reports)
    }

    async fn index_report(&self, url: &str) -> Result<Option<IndexReport>, Error> {
        Ok(self
            .indexes
            .read()
            .ok()
            .and_then(|indexes| indexes.get(url).map(|index| index.report.clone())))
    }

    async fn adopt_parameter(&self, spec: &ParameterSpec) -> Result<(), Error> {
        let mut indexes = self
            .indexes
            .write()
            .map_err(|_| Error::Internal("index lock poisoned".to_owned()))?;
        indexes
            .entry(spec.url.clone())
            .or_insert_with(|| ParamIndex {
                entries: HashMap::new(),
                report: IndexReport::empty(&spec.url),
            });
        Ok(())
    }

    async fn history(
        &self,
        scope: &HistoryScope,
        query: &HistoryQuery,
    ) -> Result<HistoryPage, Error> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let mut matches: Vec<ResourceEnvelope> = match scope {
            HistoryScope::Instance(resource_type, id) => {
                let versions = guard.get(id).ok_or(Error::NotFound)?;
                if versions
                    .first()
                    .is_none_or(|first| first.resource_type() != *resource_type)
                {
                    return Err(Error::NotFound);
                }
                versions
                    .iter()
                    .filter(|entry| query.keeps(entry))
                    .cloned()
                    .collect()
            }
            HistoryScope::Type(resource_type) => guard
                .values()
                .flatten()
                .filter(|entry| entry.resource_type() == *resource_type && query.keeps(entry))
                .cloned()
                .collect(),
            HistoryScope::System => guard
                .values()
                .flatten()
                .filter(|entry| query.keeps(entry))
                .cloned()
                .collect(),
        };
        matches.sort_by(|left, right| {
            left.last_updated()
                .key()
                .cmp(&right.last_updated().key())
                .then_with(|| left.id().as_str().cmp(right.id().as_str()))
                .then_with(|| version_number(left).cmp(&version_number(right)))
        });
        if matches!(query.order, HistoryOrder::Newest) {
            matches.reverse();
        }
        let total = matches.len();
        let entries = matches
            .into_iter()
            .skip(query.offset)
            .take(query.count)
            .collect();
        Ok(HistoryPage {
            entries,
            total,
            offset: query.offset,
        })
    }

    async fn begin(&self) -> Result<Arc<dyn StoreScope>, Error> {
        let hold = Arc::clone(&self.gate).lock_owned().await;
        let resources = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?
            .clone();
        let indexes = self
            .indexes
            .read()
            .map_err(|_| Error::Internal("index lock poisoned".to_owned()))?
            .clone();
        Ok(Arc::new(MemoryScope {
            store: Arc::new(self.sharing(true)),
            state: std::sync::Mutex::new(Some(Undo {
                resources,
                indexes,
                hold,
            })),
        }))
    }

    async fn health(&self) -> Result<(), Error> {
        match self.inner.read() {
            Ok(_) => Ok(()),
            Err(_) => Err(Error::Internal("store lock poisoned".to_owned())),
        }
    }

    async fn update(
        &self,
        envelope: ResourceEnvelope,
        expected_version: Option<&VersionId>,
    ) -> Result<ResourceEnvelope, Error> {
        let _hold = self.hold().await;
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let versions = match guard.get_mut(envelope.id()) {
            Some(versions) => versions,
            None => return Err(Error::NotFound),
        };
        let current = match versions.last() {
            Some(current) => current.clone(),
            None => return Err(Error::Internal("empty version list".to_owned())),
        };
        if let Some(expected) = expected_version {
            if current.version_id() != expected {
                return Err(Error::VersionConflict);
            }
        }
        if current.resource_type() != envelope.resource_type() {
            return Err(Error::InvalidEnvelope(format!(
                "resource type mismatch: expected {:?} found {:?}",
                current.resource_type().as_str(),
                envelope.resource_type().as_str()
            )));
        }
        if !current.is_deleted() && envelope.content_eq(&current) {
            return Ok(current);
        }
        let stored = envelope.stored_with(next_version(current.version_id())?, (self.clock)())?;
        versions.push(stored.clone());
        Ok(stored)
    }

    async fn restore_version(&self, envelope: ResourceEnvelope) -> Result<bool, Error> {
        let _hold = self.hold().await;
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        if let Some(versions) = guard.get(envelope.id()) {
            if let Some(held) = versions
                .iter()
                .find(|version| version.version_id() == envelope.version_id())
            {
                let same = held.is_deleted() == envelope.is_deleted()
                    && held.last_updated() == envelope.last_updated()
                    && held.content_eq(&envelope);
                if same {
                    return Ok(false);
                }
                return Err(repeated(&envelope));
            }
        }
        guard
            .entry(envelope.id().clone())
            .or_default()
            .push(envelope);
        Ok(true)
    }

    async fn delete(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        let _hold = self.hold().await;
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let versions = guard.get_mut(id).ok_or(Error::NotFound)?;
        let current = versions.last().ok_or(Error::NotFound)?.clone();
        if current.is_deleted() {
            return Err(Error::Deleted);
        }
        let marker = ResourceEnvelope::deleted_marker(
            current.version(),
            current.resource_type(),
            id.clone(),
            next_version(current.version_id())?,
            (self.clock)(),
        );
        versions.push(marker.clone());
        Ok(marker)
    }

    async fn hard_delete(&self, id: &ResourceId) -> Result<(), Error> {
        let _hold = self.hold().await;
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        match guard.remove(id) {
            Some(_) => Ok(()),
            None => Err(Error::NotFound),
        }
    }

    async fn purge_history(&self, id: &ResourceId) -> Result<usize, Error> {
        let _hold = self.hold().await;
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let versions = guard.get_mut(id).ok_or(Error::NotFound)?;
        let purged = versions.len().saturating_sub(1);
        if let Some(current) = versions.last().cloned() {
            *versions = vec![current];
        }
        Ok(purged)
    }
}

pub struct MemoryScope {
    store: Arc<MemoryStore>,
    state: std::sync::Mutex<Option<Undo>>,
}

struct Undo {
    resources: StoreMap,
    indexes: IndexMap,
    hold: OwnedMutexGuard<()>,
}

impl MemoryScope {
    fn settle(&self, restore: bool) -> Result<(), Error> {
        let taken = self
            .state
            .lock()
            .map_err(|_| Error::Internal("scope lock poisoned".to_owned()))?
            .take();
        let Some(undo) = taken else {
            return Ok(());
        };
        if restore {
            *self
                .store
                .inner
                .write()
                .map_err(|_| Error::Internal("store lock poisoned".to_owned()))? = undo.resources;
            *self
                .store
                .indexes
                .write()
                .map_err(|_| Error::Internal("index lock poisoned".to_owned()))? = undo.indexes;
        }
        drop(undo.hold);
        Ok(())
    }
}

impl Drop for MemoryScope {
    fn drop(&mut self) {
        let _ = self.settle(true);
    }
}

#[async_trait]
impl StoreScope for MemoryScope {
    fn store(&self) -> Arc<dyn ResourceStore> {
        Arc::clone(&self.store) as Arc<dyn ResourceStore>
    }

    async fn commit(&self) -> Result<(), Error> {
        self.settle(false)
    }

    async fn rollback(&self) -> Result<(), Error> {
        self.settle(true)
    }
}
