use crate::handler::{JobContext, JobHandler, Unit, UnitOutcome};
use crate::{payload, report};
use async_trait::async_trait;
use fhir_core::search::{Compartment, Filter};
use fhir_core::{Error, FhirInstant, InstantKey, ResourceEnvelope, ResourceId, ResourceType};
use fhir_store::{
    system_clock, BulkStore, Clock, HistoryOrder, HistoryQuery, HistoryScope, JobKind, Output,
    ResourceStore, SearchQuery,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

const ROOT: &str = "Patient";

const PROVENANCE: &str = "Provenance";

const LATEST: &str = "LatestProvenanceResources";

const RELEVANT: &str = "RelevantProvenanceResources";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportScope {
    System,
    Patient(Option<ResourceId>),
    Group(ResourceId),
}

#[derive(Debug, Clone)]
pub struct TypeFilter {
    pub resource_type: ResourceType,
    pub filters: Vec<Filter>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AssociatedData {
    pub latest: bool,
    pub relevant: bool,
}

impl AssociatedData {
    pub fn parse(names: &[String]) -> Result<AssociatedData, Error> {
        let mut held = AssociatedData::default();
        for name in names {
            match name.as_str() {
                LATEST => held.latest = true,
                RELEVANT => held.relevant = true,
                other => {
                    return Err(Error::UnsupportedParameter(format!(
                        "includeAssociatedData {other:?} is not carried"
                    )))
                }
            }
        }
        Ok(held)
    }

    pub fn requested(&self) -> bool {
        self.latest || self.relevant
    }

    fn to_value(self) -> Value {
        let mut held = Vec::new();
        if self.latest {
            held.push(LATEST.to_owned());
        }
        if self.relevant {
            held.push(RELEVANT.to_owned());
        }
        Value::Array(held.into_iter().map(Value::String).collect())
    }
}

fn narrowing(text: &str) -> Result<TypeFilter, Error> {
    let (head, query) = text.split_once('?').unwrap_or((text, ""));
    let resource_type = head.trim().parse::<ResourceType>()?;
    let mut filters = Vec::new();
    for pair in query.split('&').filter(|part| !part.trim().is_empty()) {
        let (name, raw) = pair.split_once('=').ok_or_else(|| {
            Error::InvalidParameter(format!("_typeFilter {pair:?} carries no value"))
        })?;
        if name.contains(':') || name.contains('.') {
            return Err(Error::UnsupportedParameter(format!("_typeFilter {name:?}")));
        }
        let def = fhir_core::search::lookup(Some(resource_type), name)
            .ok_or_else(|| Error::UnsupportedParameter(format!("_typeFilter {name:?}")))?;
        let values = raw
            .split(',')
            .filter(|part| !part.is_empty())
            .map(|part| def.value(part))
            .collect::<Result<Vec<_>, Error>>()?;
        filters.push(Filter::new(name, def.target.clone(), values));
    }
    Ok(TypeFilter {
        resource_type,
        filters,
    })
}

#[derive(Debug, Clone)]
pub struct Anonymization {
    pub collection: ResourceType,
    pub config: ResourceId,
    pub etag: Option<String>,
}

impl Anonymization {
    fn reference(&self) -> String {
        format!("{}/{}", self.collection.as_str(), self.config.as_str())
    }
}

fn anonymization(payload: &Value) -> Result<Option<Anonymization>, Error> {
    let Some(config) = payload::text(payload, "_anonymizationConfig") else {
        return Ok(None);
    };
    let collection = payload::text(payload, "_anonymizationConfigCollectionReference")
        .unwrap_or_else(|| "Basic".to_owned());
    Ok(Some(Anonymization {
        collection: collection.parse::<ResourceType>()?,
        config: ResourceId::parse(&config)?,
        etag: payload::text(payload, "_anonymizationConfigEtag"),
    }))
}

fn redactions(body: &Value) -> Vec<String> {
    fhir_core::search::select(body, "parameter")
        .into_iter()
        .flat_map(|parameter| match parameter {
            Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        })
        .filter(|parameter| parameter.get("name").and_then(Value::as_str) == Some("redact"))
        .filter_map(|parameter| {
            parameter
                .get("valueString")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn redacted(body: &mut Value, resource_type: ResourceType, paths: &[String]) {
    for path in paths {
        let (head, rest) = path.split_once('.').unwrap_or(("", path.as_str()));
        if !head.is_empty() && head != resource_type.as_str() {
            continue;
        }
        remove(body, rest);
    }
}

fn remove(body: &mut Value, path: &str) {
    let Some(map) = body.as_object_mut() else {
        return;
    };
    match path.split_once('.') {
        None => {
            map.remove(path);
        }
        Some((head, rest)) => {
            if let Some(nested) = map.get_mut(head) {
                match nested {
                    Value::Array(items) => items.iter_mut().for_each(|item| remove(item, rest)),
                    other => remove(other, rest),
                }
            }
        }
    }
}
#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub scope: ExportScope,
    pub types: Vec<ResourceType>,
    pub filters: Vec<TypeFilter>,
    pub narrowings: Vec<String>,
    pub since: Option<FhirInstant>,
    pub till: FhirInstant,
    pub container: String,
    pub anonymization: Option<Anonymization>,
    pub associated: AssociatedData,
    pub exporting: Vec<ResourceType>,
}

fn format_of(payload: &Value) -> Result<String, Error> {
    match payload::text(payload, "_outputFormat").or_else(|| payload::text(payload, "outputFormat"))
    {
        None => Ok(fhir_store::NDJSON.to_owned()),
        Some(found) => fhir_store::output_format(&found),
    }
}

fn scope_of(payload: &Value) -> Result<ExportScope, Error> {
    let named = payload::text(payload, "scope").unwrap_or_else(|| "system".to_owned());
    let id = match payload::text(payload, "id") {
        Some(found) => Some(ResourceId::parse(&found)?),
        None => None,
    };
    match named.as_str() {
        "system" => Ok(ExportScope::System),
        "patient" => Ok(ExportScope::Patient(id)),
        "group" => match id {
            Some(found) => Ok(ExportScope::Group(found)),
            None => Err(Error::InvalidParameter(
                "a group export names no group".to_owned(),
            )),
        },
        other => Err(Error::UnsupportedParameter(format!("scope {other:?}"))),
    }
}

impl ExportRequest {
    pub fn parse(payload: &str, fallback: &FhirInstant) -> Result<ExportRequest, Error> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|error| Error::InvalidJson(error.to_string()))?;
        let _ = format_of(&payload)?;
        let mut names = payload::listed(&payload, "_type");
        if names.is_empty() {
            names = payload::listed(&payload, "types");
        }
        let till =
            match payload::text(&payload, "_till").or_else(|| payload::text(&payload, "till")) {
                Some(found) => FhirInstant::parse(&found)?,
                None => fallback.clone(),
            };
        let container = payload::text(&payload, "_container")
            .or_else(|| payload::text(&payload, "container"))
            .unwrap_or_default();
        let since =
            match payload::text(&payload, "_since").or_else(|| payload::text(&payload, "since")) {
                Some(found) => Some(FhirInstant::parse(&found)?),
                None => None,
            };
        let narrowings = payload::listed(&payload, "_typeFilter");
        let filters = narrowings
            .iter()
            .map(|text| narrowing(text))
            .collect::<Result<Vec<TypeFilter>, Error>>()?;
        Ok(ExportRequest {
            scope: scope_of(&payload)?,
            types: payload::resource_types(&names)?,
            filters,
            narrowings,
            since,
            till,
            container,
            anonymization: anonymization(&payload)?,
            associated: AssociatedData::parse(&payload::listed(&payload, "includeAssociatedData"))?,
            exporting: payload::resource_types(&payload::listed(&payload, "_exporting"))?,
        })
    }

    fn to_value(&self, resource_type: &ResourceType, exporting: &[ResourceType]) -> Value {
        let (scope, id) = match &self.scope {
            ExportScope::System => ("system", None),
            ExportScope::Patient(id) => ("patient", id.as_ref().map(|id| id.as_str().to_owned())),
            ExportScope::Group(id) => ("group", Some(id.as_str().to_owned())),
        };
        let mut carried = Map::new();
        carried.insert("scope".to_owned(), Value::String(scope.to_owned()));
        if let Some(id) = id {
            carried.insert("id".to_owned(), Value::String(id));
        }
        carried.insert(
            "_type".to_owned(),
            Value::Array(vec![Value::String(resource_type.as_str().to_owned())]),
        );
        if resource_type.as_str() == PROVENANCE && self.associated.requested() {
            carried.insert(
                "_exporting".to_owned(),
                Value::Array(
                    exporting
                        .iter()
                        .map(|kind| Value::String(kind.as_str().to_owned()))
                        .collect(),
                ),
            );
            carried.insert(
                "includeAssociatedData".to_owned(),
                self.associated.to_value(),
            );
        }
        carried.insert(
            "_till".to_owned(),
            Value::String(self.till.as_str().to_owned()),
        );
        carried.insert(
            "_container".to_owned(),
            Value::String(self.container.clone()),
        );
        if let Some(since) = &self.since {
            carried.insert(
                "_since".to_owned(),
                Value::String(since.as_str().to_owned()),
            );
        }
        if let Some(anonymization) = &self.anonymization {
            carried.insert(
                "_anonymizationConfig".to_owned(),
                Value::String(anonymization.config.as_str().to_owned()),
            );
            carried.insert(
                "_anonymizationConfigCollectionReference".to_owned(),
                Value::String(anonymization.collection.as_str().to_owned()),
            );
            if let Some(etag) = &anonymization.etag {
                carried.insert(
                    "_anonymizationConfigEtag".to_owned(),
                    Value::String(etag.clone()),
                );
            }
        }
        carried.insert(
            "_typeFilter".to_owned(),
            Value::Array(
                self.narrowings
                    .iter()
                    .map(|text| Value::String(text.clone()))
                    .collect(),
            ),
        );
        Value::Object(carried)
    }

    fn failure_file(&self, resource_type: &ResourceType) -> String {
        match self.container.is_empty() {
            true => format!("{}-failures.ndjson", resource_type.as_str()),
            false => format!(
                "{}/{}-failures.ndjson",
                self.container,
                resource_type.as_str()
            ),
        }
    }

    fn file(&self, resource_type: &ResourceType) -> String {
        match self.container.is_empty() {
            true => format!("{}.ndjson", resource_type.as_str()),
            false => format!("{}/{}.ndjson", self.container, resource_type.as_str()),
        }
    }
}

fn body_of(envelope: &ResourceEnvelope) -> Result<Value, Error> {
    serde_json::from_slice(envelope.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

async fn snapshot(
    store: &dyn ResourceStore,
    resource_type: ResourceType,
    till: &FhirInstant,
) -> Result<Vec<ResourceEnvelope>, Error> {
    let query = HistoryQuery {
        order: HistoryOrder::Oldest,
        ..HistoryQuery::default()
    };
    let page = store
        .history(&HistoryScope::Type(resource_type), &query)
        .await?;
    let mut current: BTreeMap<String, ResourceEnvelope> = BTreeMap::new();
    let point = till.key();
    for entry in page.entries {
        if entry.last_updated().key() > point {
            continue;
        }
        current.insert(entry.id().as_str().to_owned(), entry);
    }
    Ok(current
        .into_values()
        .filter(|entry| !entry.is_deleted())
        .collect())
}

async fn every_type(store: &dyn ResourceStore) -> Result<Vec<ResourceType>, Error> {
    let page = store.search(&SearchQuery::default()).await?;
    let mut found: Vec<ResourceType> = Vec::new();
    for entry in page.entries {
        let resource_type = entry.resource_type();
        if !found.contains(&resource_type) {
            found.push(resource_type);
        }
    }
    found.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(found)
}

fn gathered_types() -> Vec<&'static str> {
    fhir_core::search::compartment::definition(ROOT)
        .map(|def| def.types())
        .unwrap_or_default()
}

async fn members(
    store: &dyn ResourceStore,
    id: &ResourceId,
    till: &FhirInstant,
) -> Result<Vec<ResourceId>, Error> {
    let group_type = "Group".parse::<ResourceType>()?;
    let group = snapshot(store, group_type, till)
        .await?
        .into_iter()
        .find(|entry| entry.id() == id)
        .ok_or(Error::NotFound)?;
    let body = body_of(&group)?;
    let mut found = Vec::new();
    for element in fhir_core::search::select(&body, "member.entity") {
        for pointer in fhir_core::search::pointers(element) {
            let bare = pointer.rsplit('/').next().unwrap_or_default();
            if let Ok(id) = ResourceId::parse(bare) {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
    }
    Ok(found)
}

fn narrowed(
    request: &ExportRequest,
    resource_type: ResourceType,
    entry: &ResourceEnvelope,
    body: &Value,
) -> bool {
    let mut wanted = request
        .filters
        .iter()
        .filter(|found| found.resource_type == resource_type)
        .peekable();
    if wanted.peek().is_none() {
        return true;
    }
    wanted.any(|found| {
        found
            .filters
            .iter()
            .all(|filter| filter.matches(entry.id(), entry.last_updated(), body))
    })
}

fn gathered(roots: &[ResourceId], resource_type: ResourceType, body: &Value) -> bool {
    let kind = match ROOT.parse::<ResourceType>() {
        Ok(kind) => kind,
        Err(_) => return false,
    };
    roots.iter().any(|id| {
        fhir_core::search::compartment::contains(
            &Compartment {
                kind,
                id: id.clone(),
            },
            resource_type,
            body,
        )
    })
}

fn selected(
    request: &ExportRequest,
    resource_type: ResourceType,
    entry: &ResourceEnvelope,
    body: &Value,
    roots: Option<&[ResourceId]>,
) -> bool {
    !roots.is_some_and(|roots| !gathered(roots, resource_type, body))
        && !request
            .since
            .as_ref()
            .is_some_and(|since| entry.last_updated().key() < since.key())
        && narrowed(request, resource_type, entry, body)
}

fn targets_of(body: &Value) -> BTreeSet<(String, String)> {
    let mut held = BTreeSet::new();
    for pointer in fhir_core::search::select(body, "target")
        .iter()
        .flat_map(|element| fhir_core::search::pointers(element))
    {
        let mut segments = pointer.split('/').collect::<Vec<&str>>();
        let id = segments.pop().unwrap_or_default();
        let resource_type = segments.pop().unwrap_or_default();
        if !id.is_empty() && !resource_type.is_empty() {
            held.insert((resource_type.to_owned(), id.to_owned()));
        }
    }
    held
}

async fn exported_ids(
    store: &dyn ResourceStore,
    request: &ExportRequest,
    roots: Option<&[ResourceId]>,
    exporting: &[ResourceType],
) -> Result<HashMap<String, BTreeSet<String>>, Error> {
    let mut held = HashMap::new();
    for resource_type in exporting {
        let key = resource_type.as_str().to_owned();
        let mut ids = BTreeSet::new();
        for entry in snapshot(store, *resource_type, &request.till).await? {
            if let Ok(parsed) = body_of(&entry) {
                if selected(request, *resource_type, &entry, &parsed, roots) {
                    ids.insert(entry.id().as_str().to_owned());
                }
            }
        }
        held.insert(key, ids);
    }
    Ok(held)
}

fn latest_only(entries: Vec<ResourceEnvelope>) -> Vec<ResourceEnvelope> {
    let mut winners: BTreeMap<(String, String), ResourceEnvelope> = BTreeMap::new();
    for entry in entries {
        let Ok(body) = body_of(&entry) else {
            continue;
        };
        let targets = targets_of(&body);
        for target in targets {
            let previous = winners.get(&target);
            let wins = match previous {
                Some(found) => latest_key(found) <= latest_key(&entry),
                None => true,
            };
            if wins {
                winners.insert(target, entry.clone());
            }
        }
    }
    winners.into_values().collect()
}

fn latest_key(entry: &ResourceEnvelope) -> InstantKey {
    body_of(entry)
        .ok()
        .and_then(|body| {
            body.get("recorded")
                .and_then(|recorded| recorded.as_str())
                .and_then(|recorded| FhirInstant::parse(recorded).ok())
        })
        .unwrap_or_else(|| entry.last_updated().clone())
        .key()
}

pub struct ExportJob {
    store: Arc<dyn ResourceStore>,
    sink: Arc<dyn BulkStore>,
    clock: Clock,
}

impl ExportJob {
    pub fn new(store: Arc<dyn ResourceStore>, sink: Arc<dyn BulkStore>) -> ExportJob {
        ExportJob {
            store,
            sink,
            clock: system_clock(),
        }
    }

    pub fn with_clock(self, clock: Clock) -> ExportJob {
        ExportJob { clock, ..self }
    }

    async fn planned(&self, request: &ExportRequest) -> Result<Vec<ResourceType>, Error> {
        let mut types = match request.types.is_empty() {
            false => request.types.clone(),
            true => every_type(self.store.as_ref()).await?,
        };
        if !matches!(request.scope, ExportScope::System) {
            let gathered = gathered_types();
            types.retain(|found| gathered.contains(&found.as_str()));
        }
        if request.associated.requested() && !types.iter().any(|found| found.as_str() == PROVENANCE)
        {
            types.push(PROVENANCE.parse()?);
        }
        Ok(types)
    }

    async fn rules(&self, request: &ExportRequest) -> Result<Option<Vec<String>>, Error> {
        let Some(anonymization) = &request.anonymization else {
            return Ok(None);
        };
        let held = snapshot(self.store.as_ref(), anonymization.collection, &request.till)
            .await?
            .into_iter()
            .find(|entry| entry.id() == &anonymization.config)
            .ok_or_else(|| {
                Error::InvalidParameter(format!(
                    "the configuration {:?} is not held",
                    anonymization.reference()
                ))
            })?;
        if let Some(etag) = &anonymization.etag {
            if held.version_id().as_str() != etag {
                return Err(Error::InvalidParameter(format!(
                    "the configuration {:?} stands at {:?}, not {etag:?}",
                    anonymization.reference(),
                    held.version_id().as_str()
                )));
            }
        }
        Ok(Some(redactions(&body_of(&held)?)))
    }

    async fn roots(&self, request: &ExportRequest) -> Result<Option<Vec<ResourceId>>, Error> {
        match &request.scope {
            ExportScope::System => Ok(None),
            ExportScope::Patient(None) => Ok(None),
            ExportScope::Patient(Some(id)) => Ok(Some(vec![id.clone()])),
            ExportScope::Group(id) => {
                Ok(Some(members(self.store.as_ref(), id, &request.till).await?))
            }
        }
    }

    async fn associated(
        &self,
        job: &JobContext,
        request: &ExportRequest,
        roots: Option<&[ResourceId]>,
    ) -> Result<UnitOutcome, Error> {
        let exporting = match request.exporting.is_empty() {
            false => &request.exporting,
            true => &request.types,
        };
        let exported = exported_ids(self.store.as_ref(), request, roots, exporting).await?;
        let mut held: Vec<ResourceEnvelope> = Vec::new();
        for entry in snapshot(self.store.as_ref(), PROVENANCE.parse()?, &request.till).await? {
            let parsed = match body_of(&entry) {
                Ok(parsed) => parsed,
                Err(_) => continue,
            };
            let targeted = targets_of(&parsed).into_iter().any(|(resource_type, id)| {
                exported
                    .get(&resource_type)
                    .is_some_and(|ids| ids.contains(&id))
            });
            if !targeted {
                continue;
            }
            held.push(entry);
        }
        if request.associated.latest && !request.associated.relevant {
            held = latest_only(held);
        }
        let mut body = Vec::new();
        for entry in &held {
            if let Ok(parsed) = body_of(entry) {
                body.extend_from_slice(&serde_json::to_vec(&parsed).unwrap_or_default());
                body.push(b'\n');
            }
        }
        let outcome = UnitOutcome {
            handled: held.len() as u64,
            ..Default::default()
        };
        if outcome.handled > 0 {
            let resource_type = PROVENANCE.parse()?;
            let output = Output::new(
                request.file(&resource_type),
                resource_type.as_str(),
                outcome.handled,
            );
            self.sink.write(&job.id, &output, &body).await?;
        }
        Ok(outcome)
    }
}

#[async_trait]
impl JobHandler for ExportJob {
    fn kind(&self) -> JobKind {
        JobKind::Export
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        let request = ExportRequest::parse(&job.payload, &(self.clock)())?;
        let _ = self.rules(&request).await?;
        let exporting = self.planned(&request).await?;
        Ok(exporting
            .iter()
            .map(|resource_type| {
                Unit::new(
                    resource_type.as_str().to_owned(),
                    request.to_value(resource_type, &exporting).to_string(),
                )
            })
            .collect())
    }

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let request = ExportRequest::parse(&unit.detail, &(self.clock)())?;
        let resource_type = unit.label.parse::<ResourceType>()?;
        let roots = self.roots(&request).await?;
        if resource_type.as_str() == PROVENANCE && request.associated.requested() {
            return self.associated(job, &request, roots.as_deref()).await;
        }
        let rules = self.rules(&request).await?;
        let mut body = Vec::new();
        let mut outcome = UnitOutcome::default();
        let mut held: Vec<String> = Vec::new();
        for entry in snapshot(self.store.as_ref(), resource_type, &request.till).await? {
            held.push(entry.id().as_str().to_owned());
            let parsed = match body_of(&entry) {
                Ok(parsed) => parsed,
                Err(error) => {
                    outcome.failures.push(format!(
                        "{}/{}: {error}",
                        resource_type.as_str(),
                        entry.id().as_str()
                    ));
                    continue;
                }
            };
            if !selected(&request, resource_type, &entry, &parsed, roots.as_deref()) {
                continue;
            }
            let mut parsed = parsed;
            if let Some(rules) = &rules {
                redacted(&mut parsed, resource_type, rules);
            }
            body.extend_from_slice(&serde_json::to_vec(&parsed).unwrap_or_default());
            body.push(b'\n');
            outcome.handled += 1;
        }
        if let Some(roots) = &roots {
            if resource_type == ROOT.parse::<ResourceType>()? {
                for root in roots {
                    if !held.iter().any(|found| found == root.as_str()) {
                        outcome
                            .failures
                            .push(format!("{ROOT}/{}: the member is not held", root.as_str()));
                    }
                }
            }
        }
        if outcome.handled > 0 {
            let output = Output::new(
                request.file(&resource_type),
                resource_type.as_str(),
                outcome.handled,
            );
            self.sink.write(&job.id, &output, &body).await?;
        }
        report::record_failures(
            self.sink.as_ref(),
            &job.id,
            &request.failure_file(&resource_type),
            &outcome.failures,
        )
        .await?;
        outcome.detail.insert(
            "transactionTime".to_owned(),
            Value::String(request.till.as_str().to_owned()),
        );
        if let Some(anonymization) = &request.anonymization {
            outcome
                .detail
                .insert("anonymized".to_owned(), Value::Bool(true));
            outcome.detail.insert(
                "anonymizationConfig".to_owned(),
                Value::String(anonymization.reference()),
            );
            if let Some(etag) = &anonymization.etag {
                outcome.detail.insert(
                    "anonymizationConfigEtag".to_owned(),
                    Value::String(etag.clone()),
                );
            }
        }
        Ok(outcome)
    }
}
