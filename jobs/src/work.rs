use crate::handler::{JobContext, JobHandler, Unit, UnitOutcome};
use crate::{payload, report};
use async_trait::async_trait;
use fhir_core::search::ParameterSpec;
use fhir_core::{Error, FhirVersion, Patch, ResourceEnvelope, ResourceId, ResourceType};
use fhir_store::{
    BulkStore, HistoryOrder, HistoryQuery, HistoryScope, InteractionEntry, Interactions, JobKind,
    ResourceStore, SearchQuery,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

async fn latest_of(
    store: &dyn ResourceStore,
    scope: &HistoryScope,
) -> Result<Vec<ResourceEnvelope>, Error> {
    let query = HistoryQuery {
        order: HistoryOrder::Oldest,
        ..HistoryQuery::default()
    };
    let page = store.history(scope, &query).await?;
    let mut current: BTreeMap<String, ResourceEnvelope> = BTreeMap::new();
    for entry in page.entries {
        current.insert(entry.id().as_str().to_owned(), entry);
    }
    Ok(current.into_values().collect())
}

async fn marked_of(
    store: &dyn ResourceStore,
    resource_type: ResourceType,
) -> Result<Vec<ResourceEnvelope>, Error> {
    Ok(latest_of(store, &HistoryScope::Type(resource_type))
        .await?
        .into_iter()
        .filter(ResourceEnvelope::is_deleted)
        .collect())
}

async fn every_recorded_type(store: &dyn ResourceStore) -> Result<Vec<ResourceType>, Error> {
    let mut found: Vec<ResourceType> = Vec::new();
    for entry in latest_of(store, &HistoryScope::System).await? {
        let resource_type = entry.resource_type();
        if !found.contains(&resource_type) {
            found.push(resource_type);
        }
    }
    found.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(found)
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

async fn current_of(
    store: &dyn ResourceStore,
    label: &str,
) -> Result<Vec<ResourceEnvelope>, Error> {
    let resource_type = label.parse::<ResourceType>()?;
    let page = store.search(&SearchQuery::of_type(resource_type)).await?;
    Ok(page.entries)
}

fn positioned(rows: impl IntoIterator<Item = (usize, String)>) -> Vec<Unit> {
    rows.into_iter()
        .map(|(position, row)| Unit::new(format!("row {position}"), row))
        .collect()
}

fn described(payload: &str) -> Option<Vec<Unit>> {
    let parsed = serde_json::from_str::<Value>(payload).ok()?;
    let carried = parsed.as_object()?;
    if carried.contains_key("resourceType") {
        return None;
    }
    let rows = carried
        .get("resources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Some(positioned(
        rows.iter()
            .enumerate()
            .map(|(position, row)| (position, row.to_string())),
    ))
}

fn delimited(payload: &str) -> Vec<Unit> {
    positioned(
        payload
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(position, line)| (position, line.trim().to_owned())),
    )
}

pub const IMPORT_FAILURES: &str = "import";

pub struct ImportJob {
    store: Arc<dyn ResourceStore>,
    version: FhirVersion,
    sink: Option<Arc<dyn BulkStore>>,
    itemised: std::sync::Mutex<BTreeMap<String, Vec<String>>>,
}

impl ImportJob {
    pub fn new(store: Arc<dyn ResourceStore>, version: FhirVersion) -> ImportJob {
        ImportJob {
            store,
            version,
            sink: None,
            itemised: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    pub fn reporting(self, sink: Arc<dyn BulkStore>) -> ImportJob {
        ImportJob {
            sink: Some(sink),
            ..self
        }
    }

    async fn itemise(&self, job: &JobContext, failures: &[String]) -> Result<(), Error> {
        let Some(sink) = &self.sink else {
            return Ok(());
        };
        if failures.is_empty() {
            return Ok(());
        }
        let held = {
            let mut carried = self
                .itemised
                .lock()
                .map_err(|_| Error::Internal("the import report is poisoned".to_owned()))?;
            let held = carried.entry(job.id.as_str().to_owned()).or_default();
            held.extend_from_slice(failures);
            held.clone()
        };
        report::record_failures(
            sink.as_ref(),
            &job.id,
            &report::failure_file("", IMPORT_FAILURES),
            &held,
        )
        .await
    }

    async fn versioned(&self, envelope: ResourceEnvelope) -> Result<bool, Error> {
        let held = self.store.read(envelope.id()).await?;
        let stored = self.store.update(envelope, None).await?;
        Ok(stored.version_id() != held.version_id())
    }
}

#[async_trait]
impl JobHandler for ImportJob {
    fn kind(&self) -> JobKind {
        JobKind::Import
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        Ok(described(&job.payload).unwrap_or_else(|| delimited(&job.payload)))
    }

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let envelope = match ResourceEnvelope::parse_supplied(self.version, unit.detail.as_bytes())
        {
            Ok(envelope) => envelope,
            Err(error) => {
                let failures = vec![format!("{}: {error}", unit.label)];
                self.itemise(job, &failures).await?;
                return Ok(UnitOutcome {
                    handled: 0,
                    failures,
                    ..UnitOutcome::default()
                });
            }
        };
        let written = match self.store.create(envelope.clone()).await {
            Ok(_) => Ok(true),
            Err(Error::Duplicate(_)) => self.versioned(envelope).await,
            Err(error) => Err(error),
        };
        match written {
            Ok(true) => Ok(UnitOutcome::handled(1)),
            Ok(false) => Ok(UnitOutcome {
                unchanged: 1,
                ..UnitOutcome::default()
            }),
            Err(error) => {
                let failures = vec![format!("{}: {error}", unit.label)];
                self.itemise(job, &failures).await?;
                Ok(UnitOutcome {
                    failures,
                    ..UnitOutcome::default()
                })
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct BulkDeleteRequest {
    pub types: Vec<ResourceType>,
    pub excluded: Vec<ResourceType>,
    pub max_count: Option<u64>,
    pub hard: bool,
    pub purge: bool,
    pub soft_deleted: bool,
}

impl BulkDeleteRequest {
    pub fn parse(payload: &str) -> Result<BulkDeleteRequest, Error> {
        let parsed = payload::body(payload)?;
        Ok(BulkDeleteRequest {
            types: payload::resource_types(&payload::named(&parsed, &["_type", "types"]))?,
            excluded: payload::resource_types(&payload::named(&parsed, &["_exclude", "excluded"]))?,
            max_count: payload::count(&parsed, "_maxCount")?,
            hard: payload::flagged(&parsed, &["_hardDelete", "hardDelete"]),
            purge: payload::flagged(&parsed, &["_purgeHistory", "purgeHistory"]),
            soft_deleted: payload::flagged(&parsed, &["_softDeleted", "softDeleted"]),
        })
    }

    fn to_value(&self, resource_type: &ResourceType, share: Option<u64>) -> Value {
        let mut carried = serde_json::Map::new();
        carried.insert(
            "_type".to_owned(),
            Value::Array(vec![Value::String(resource_type.as_str().to_owned())]),
        );
        if let Some(share) = share {
            carried.insert("_maxCount".to_owned(), Value::from(share));
        }
        carried.insert("hardDelete".to_owned(), Value::Bool(self.hard));
        carried.insert("purgeHistory".to_owned(), Value::Bool(self.purge));
        carried.insert("softDeleted".to_owned(), Value::Bool(self.soft_deleted));
        Value::Object(carried)
    }
}

pub struct BulkDeleteJob {
    store: Arc<dyn ResourceStore>,
    sink: Arc<dyn BulkStore>,
}

impl BulkDeleteJob {
    pub fn new(store: Arc<dyn ResourceStore>, sink: Arc<dyn BulkStore>) -> BulkDeleteJob {
        BulkDeleteJob { store, sink }
    }

    async fn covered(&self, request: &BulkDeleteRequest) -> Result<Vec<ResourceType>, Error> {
        let mut types = match request.types.is_empty() {
            false => request.types.clone(),
            true => match request.soft_deleted {
                true => every_recorded_type(self.store.as_ref()).await?,
                false => every_type(self.store.as_ref()).await?,
            },
        };
        types.retain(|found| !request.excluded.contains(found));
        Ok(types)
    }

    async fn candidates(
        &self,
        request: &BulkDeleteRequest,
        resource_type: ResourceType,
    ) -> Result<Vec<ResourceEnvelope>, Error> {
        match request.soft_deleted {
            true => marked_of(self.store.as_ref(), resource_type).await,
            false => {
                let page = self
                    .store
                    .search(&SearchQuery::of_type(resource_type))
                    .await?;
                Ok(page.entries)
            }
        }
    }

    async fn remove(
        &self,
        request: &BulkDeleteRequest,
        entry: &ResourceEnvelope,
    ) -> Result<u64, Error> {
        let id = entry.id();
        if request.soft_deleted {
            return match request.hard || !request.purge {
                true => self.store.hard_delete(id).await.map(|_| 0),
                false => self.store.purge_history(id).await.map(|gone| gone as u64),
            };
        }
        if request.hard {
            return self.store.hard_delete(id).await.map(|_| 0);
        }
        self.store.delete(id).await?;
        match request.purge {
            true => self.store.purge_history(id).await.map(|gone| gone as u64),
            false => Ok(0),
        }
    }
}

#[async_trait]
impl JobHandler for BulkDeleteJob {
    fn kind(&self) -> JobKind {
        JobKind::BulkDelete
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        let request = BulkDeleteRequest::parse(&job.payload)?;
        let mut left = request.max_count;
        let mut units = Vec::new();
        for resource_type in self.covered(&request).await? {
            let share = match left {
                None => None,
                Some(0) => break,
                Some(budget) => {
                    let held = self.candidates(&request, resource_type).await?.len() as u64;
                    let share = held.min(budget);
                    left = Some(budget - share);
                    Some(share)
                }
            };
            if share == Some(0) {
                continue;
            }
            units.push(Unit::new(
                resource_type.as_str().to_owned(),
                request.to_value(&resource_type, share).to_string(),
            ));
        }
        Ok(units)
    }

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let request = BulkDeleteRequest::parse(&unit.detail)?;
        let resource_type = unit.label.parse::<ResourceType>()?;
        let cap = request.max_count.unwrap_or(u64::MAX).min(usize::MAX as u64) as usize;
        let mut outcome = UnitOutcome::default();
        let mut purged = 0;
        for entry in self
            .candidates(&request, resource_type)
            .await?
            .into_iter()
            .take(cap)
        {
            match self.remove(&request, &entry).await {
                Ok(gone) => {
                    outcome.handled += 1;
                    purged += gone;
                }
                Err(Error::Deleted) | Err(Error::NotFound) => {}
                Err(error) => outcome.failures.push(format!(
                    "{}/{}: {error}",
                    resource_type.as_str(),
                    entry.id().as_str()
                )),
            }
        }
        report::record_failures(
            self.sink.as_ref(),
            &job.id,
            &report::failure_file("", resource_type.as_str()),
            &outcome.failures,
        )
        .await?;
        outcome.detail.insert(
            resource_type.as_str().to_owned(),
            serde_json::json!({"deleted": outcome.handled, "purged": purged}),
        );
        Ok(outcome)
    }
}

#[derive(Debug, Clone)]
pub struct BulkUpdateRequest {
    pub types: Vec<ResourceType>,
    pub excluded: Vec<ResourceType>,
    pub max_count: Option<u64>,
    pub patch: String,
}

impl BulkUpdateRequest {
    pub fn parse(payload: &str) -> Result<BulkUpdateRequest, Error> {
        let parsed = payload::body(payload)?;
        let supplied = parsed
            .get("patch")
            .ok_or_else(|| Error::InvalidPatch("no patch was supplied".to_owned()))?
            .to_string();
        Patch::parse(supplied.as_bytes())?;
        Ok(BulkUpdateRequest {
            types: payload::resource_types(&payload::named(&parsed, &["_type", "types"]))?,
            excluded: payload::resource_types(&payload::named(&parsed, &["_exclude", "excluded"]))?,
            max_count: payload::count(&parsed, "_maxCount")?,
            patch: supplied,
        })
    }

    fn to_value(&self, resource_type: &ResourceType, share: Option<u64>) -> Value {
        let mut carried = serde_json::Map::new();
        carried.insert(
            "_type".to_owned(),
            Value::Array(vec![Value::String(resource_type.as_str().to_owned())]),
        );
        if let Some(share) = share {
            carried.insert("_maxCount".to_owned(), Value::from(share));
        }
        carried.insert(
            "patch".to_owned(),
            serde_json::from_str(&self.patch).unwrap_or(Value::Null),
        );
        Value::Object(carried)
    }
}

pub struct BulkUpdateJob {
    store: Arc<dyn ResourceStore>,
    sink: Arc<dyn BulkStore>,
}

impl BulkUpdateJob {
    pub fn new(store: Arc<dyn ResourceStore>, sink: Arc<dyn BulkStore>) -> BulkUpdateJob {
        BulkUpdateJob { store, sink }
    }

    async fn covered(&self, request: &BulkUpdateRequest) -> Result<Vec<ResourceType>, Error> {
        let mut types = match request.types.is_empty() {
            false => request.types.clone(),
            true => every_type(self.store.as_ref()).await?,
        };
        types.retain(|found| !request.excluded.contains(found));
        Ok(types)
    }
}

#[async_trait]
impl JobHandler for BulkUpdateJob {
    fn kind(&self) -> JobKind {
        JobKind::BulkUpdate
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        let request = BulkUpdateRequest::parse(&job.payload)?;
        let mut left = request.max_count;
        let mut units = Vec::new();
        for resource_type in self.covered(&request).await? {
            let share = match left {
                None => None,
                Some(0) => break,
                Some(budget) => {
                    let held = current_of(self.store.as_ref(), resource_type.as_str())
                        .await?
                        .len() as u64;
                    let share = held.min(budget);
                    left = Some(budget - share);
                    Some(share)
                }
            };
            if share == Some(0) {
                continue;
            }
            units.push(Unit::new(
                resource_type.as_str().to_owned(),
                request.to_value(&resource_type, share).to_string(),
            ));
        }
        Ok(units)
    }

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let request = BulkUpdateRequest::parse(&unit.detail)?;
        let patch = Patch::parse(request.patch.as_bytes())?;
        let resource_type = unit.label.parse::<ResourceType>()?;
        let cap = request.max_count.unwrap_or(u64::MAX).min(usize::MAX as u64) as usize;
        let mut outcome = UnitOutcome::default();
        for entry in current_of(self.store.as_ref(), &unit.label)
            .await?
            .into_iter()
            .take(cap)
        {
            let patched = patch
                .apply(entry.raw())
                .and_then(|bytes| ResourceEnvelope::parse(entry.version(), &bytes));
            let written = match patched {
                Ok(envelope) => self.store.update(envelope, None).await,
                Err(error) => Err(error),
            };
            match written {
                Ok(stored) if stored.version_id() == entry.version_id() => outcome.unchanged += 1,
                Ok(_) => outcome.handled += 1,
                Err(error) => outcome.failures.push(format!(
                    "{}/{}: {error}",
                    resource_type.as_str(),
                    entry.id().as_str()
                )),
            }
        }
        report::record_failures(
            self.sink.as_ref(),
            &job.id,
            &report::failure_file("", resource_type.as_str()),
            &outcome.failures,
        )
        .await?;
        outcome.detail.insert(
            resource_type.as_str().to_owned(),
            serde_json::json!({"patched": outcome.handled, "unchanged": outcome.unchanged}),
        );
        Ok(outcome)
    }
}

fn logical(reference: &str) -> Result<ResourceId, Error> {
    ResourceId::parse(reference.rsplit('/').next().unwrap_or_default())
}

fn safe(label: &str) -> String {
    label
        .chars()
        .map(
            |held| match held.is_ascii_alphanumeric() || held == '.' || held == '-' {
                true => held,
                false => '-',
            },
        )
        .collect()
}

#[derive(Debug, Clone)]
pub struct ReindexRequest {
    pub urls: Vec<String>,
    pub references: Vec<String>,
    pub types: Vec<ResourceType>,
}

impl ReindexRequest {
    pub fn parse(payload: &str) -> Result<ReindexRequest, Error> {
        let parsed = payload::body(payload)?;
        let references = payload::named(&parsed, &["_resource", "resources"]);
        for reference in &references {
            logical(reference)?;
        }
        Ok(ReindexRequest {
            urls: payload::named(&parsed, &["_url", "urls"]),
            references,
            types: payload::resource_types(&payload::named(&parsed, &["_type", "types"]))?,
        })
    }
}

pub struct ReindexJob {
    store: Arc<dyn ResourceStore>,
    sink: Arc<dyn BulkStore>,
}

impl ReindexJob {
    pub fn new(store: Arc<dyn ResourceStore>, sink: Arc<dyn BulkStore>) -> ReindexJob {
        ReindexJob { store, sink }
    }

    async fn matching(&self, urls: &[String]) -> Result<Vec<ParameterSpec>, Error> {
        let resource_type = "SearchParameter".parse::<ResourceType>()?;
        let page = self
            .store
            .search(&SearchQuery::of_type(resource_type))
            .await?;
        let mut specs = Vec::new();
        for entry in page.entries {
            let Ok(parsed) = serde_json::from_slice::<Value>(entry.raw()) else {
                continue;
            };
            let Ok(spec) = ParameterSpec::parse(&parsed) else {
                continue;
            };
            if spec.retired || (!urls.is_empty() && !urls.contains(&spec.url)) {
                continue;
            }
            specs.push(spec);
        }
        Ok(specs)
    }

    async fn covered(&self, request: &ReindexRequest) -> Result<Vec<ParameterSpec>, Error> {
        let mut specs = self.matching(&request.urls).await?;
        if !request.types.is_empty() {
            specs.retain(|spec| spec.base.iter().any(|base| request.types.contains(base)));
        }
        Ok(specs)
    }

    async fn one_resource(
        &self,
        detail: &Value,
        reference: &str,
        outcome: &mut UnitOutcome,
    ) -> Result<(), Error> {
        let specs = self.matching(&payload::listed(detail, "urls")).await?;
        let reports = match self
            .store
            .reindex_resource(&specs, &logical(reference)?)
            .await
        {
            Ok(reports) => reports,
            Err(error) => {
                outcome.failures.push(format!("{reference}: {error}"));
                return Ok(());
            }
        };
        let mut parameters = 0;
        for report in reports {
            if report.indexed > 0 {
                parameters += 1;
            }
            for failure in report.failures {
                outcome
                    .failures
                    .push(format!("{}: {}", failure.resource, failure.reason));
            }
        }
        if parameters > 0 {
            outcome.handled += 1;
        }
        outcome.detail.insert(
            reference.to_owned(),
            serde_json::json!({ "parameters": parameters }),
        );
        Ok(())
    }

    async fn one_parameter(&self, url: &str, outcome: &mut UnitOutcome) -> Result<(), Error> {
        let specs = self.matching(std::slice::from_ref(&url.to_owned())).await?;
        let reports = self.store.reindex(&specs).await?;
        let mut indexed = 0;
        for report in reports {
            indexed += report.indexed as u64;
            for failure in report.failures {
                outcome
                    .failures
                    .push(format!("{}: {}", failure.resource, failure.reason));
            }
        }
        outcome.handled += indexed;
        outcome
            .detail
            .insert(url.to_owned(), serde_json::json!({ "indexed": indexed }));
        Ok(())
    }
}

#[async_trait]
impl JobHandler for ReindexJob {
    fn kind(&self) -> JobKind {
        JobKind::Reindex
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        let request = ReindexRequest::parse(&job.payload)?;
        let specs = self.covered(&request).await?;
        if request.references.is_empty() {
            return Ok(specs
                .into_iter()
                .map(|spec| {
                    let detail = serde_json::json!({ "url": spec.url });
                    Unit::new(spec.url, detail.to_string())
                })
                .collect());
        }
        let urls: Vec<Value> = specs
            .iter()
            .map(|spec| Value::String(spec.url.clone()))
            .collect();
        Ok(request
            .references
            .iter()
            .map(|reference| {
                let detail = serde_json::json!({ "resource": reference, "urls": urls });
                Unit::new(reference.clone(), detail.to_string())
            })
            .collect())
    }

    async fn process(&self, job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let detail = payload::body(&unit.detail)?;
        let mut outcome = UnitOutcome::default();
        match payload::text(&detail, "resource") {
            Some(reference) => self.one_resource(&detail, &reference, &mut outcome).await?,
            None => match payload::text(&detail, "url") {
                Some(url) => self.one_parameter(&url, &mut outcome).await?,
                None => {
                    return Err(Error::InvalidParameter(
                        "a reindex unit names nothing".to_owned(),
                    ))
                }
            },
        }
        report::record_failures(
            self.sink.as_ref(),
            &job.id,
            &report::failure_file("", &safe(&unit.label)),
            &outcome.failures,
        )
        .await?;
        Ok(outcome)
    }
}

pub struct InteractionJob {
    runner: Arc<dyn Interactions>,
}

impl InteractionJob {
    pub fn new(runner: Arc<dyn Interactions>) -> InteractionJob {
        InteractionJob { runner }
    }
}

#[async_trait]
impl JobHandler for InteractionJob {
    fn kind(&self) -> JobKind {
        JobKind::Interaction
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        Ok(vec![Unit::new("interaction", job.payload.clone())])
    }

    async fn process(&self, _job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let entry = self.runner.perform(&unit.detail).await;
        let mut outcome = UnitOutcome::handled(1);
        outcome
            .detail
            .insert(InteractionEntry::RESPONSE.to_owned(), entry.bundle());
        Ok(outcome)
    }
}
