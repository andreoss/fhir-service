use crate::handler::{JobContext, JobHandler, Unit, UnitOutcome};
use async_trait::async_trait;
use fhir_core::search::ParameterSpec;
use fhir_core::{Error, FhirVersion, Patch, ResourceEnvelope, ResourceType};
use fhir_store::{JobKind, ResourceStore, SearchQuery};
use serde_json::Value;
use std::sync::Arc;

fn body(payload: &str) -> Result<Value, Error> {
    serde_json::from_str(payload).map_err(|error| Error::InvalidJson(error.to_string()))
}

fn types_of(payload: &Value) -> Result<Vec<ResourceType>, Error> {
    let Some(listed) = payload.get("types").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    listed
        .iter()
        .map(|value| match value.as_str() {
            Some(name) => name.parse::<ResourceType>(),
            None => Err(Error::InvalidResourceType(value.to_string())),
        })
        .collect()
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

async fn units_per_type(
    store: &dyn ResourceStore,
    payload: &str,
    detail: impl Fn(&ResourceType) -> String,
) -> Result<Vec<Unit>, Error> {
    let payload = body(payload)?;
    let mut types = types_of(&payload)?;
    if types.is_empty() {
        types = every_type(store).await?;
    }
    Ok(types
        .into_iter()
        .map(|resource_type| Unit::new(resource_type.as_str().to_owned(), detail(&resource_type)))
        .collect())
}

async fn current_of(store: &dyn ResourceStore, label: &str) -> Result<Vec<ResourceEnvelope>, Error> {
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

pub struct ImportJob {
    store: Arc<dyn ResourceStore>,
    version: FhirVersion,
}

impl ImportJob {
    pub fn new(store: Arc<dyn ResourceStore>, version: FhirVersion) -> ImportJob {
        ImportJob { store, version }
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

    async fn process(&self, _job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let envelope = match ResourceEnvelope::parse(self.version, unit.detail.as_bytes()) {
            Ok(envelope) => envelope,
            Err(error) => {
                return Ok(UnitOutcome {
                    handled: 0,
                    failures: vec![format!("{}: {error}", unit.label)],
                    ..UnitOutcome::default()
                })
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
            Err(error) => Ok(UnitOutcome {
                failures: vec![format!("{}: {error}", unit.label)],
                ..UnitOutcome::default()
            }),
        }
    }
}

pub struct BulkDeleteJob {
    store: Arc<dyn ResourceStore>,
}

impl BulkDeleteJob {
    pub fn new(store: Arc<dyn ResourceStore>) -> BulkDeleteJob {
        BulkDeleteJob { store }
    }
}

#[async_trait]
impl JobHandler for BulkDeleteJob {
    fn kind(&self) -> JobKind {
        JobKind::BulkDelete
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        units_per_type(self.store.as_ref(), &job.payload, |_| String::new()).await
    }

    async fn process(&self, _job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let entries = current_of(self.store.as_ref(), &unit.label).await?;
        let mut outcome = UnitOutcome::default();
        for entry in entries {
            match self.store.delete(entry.id()).await {
                Ok(_) => outcome.handled += 1,
                Err(Error::Deleted) | Err(Error::NotFound) => {}
                Err(error) => outcome
                    .failures
                    .push(format!("{}: {error}", entry.id().as_str())),
            }
        }
        Ok(outcome)
    }
}

pub struct BulkUpdateJob {
    store: Arc<dyn ResourceStore>,
}

impl BulkUpdateJob {
    pub fn new(store: Arc<dyn ResourceStore>) -> BulkUpdateJob {
        BulkUpdateJob { store }
    }
}

#[async_trait]
impl JobHandler for BulkUpdateJob {
    fn kind(&self) -> JobKind {
        JobKind::BulkUpdate
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        let parsed = body(&job.payload)?;
        let patch = parsed
            .get("patch")
            .ok_or_else(|| Error::InvalidPatch("no patch was supplied".to_owned()))?;
        Patch::parse(patch.to_string().as_bytes())?;
        let carried = patch.to_string();
        units_per_type(self.store.as_ref(), &job.payload, move |_| carried.clone()).await
    }

    async fn process(&self, _job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let patch = Patch::parse(unit.detail.as_bytes())?;
        let entries = current_of(self.store.as_ref(), &unit.label).await?;
        let mut outcome = UnitOutcome::default();
        for entry in entries {
            let patched = patch
                .apply(entry.raw())
                .and_then(|bytes| ResourceEnvelope::parse(entry.version(), &bytes));
            match patched {
                Ok(envelope) => match self.store.update(envelope, None).await {
                    Ok(_) => outcome.handled += 1,
                    Err(error) => outcome
                        .failures
                        .push(format!("{}: {error}", entry.id().as_str())),
                },
                Err(error) => outcome
                    .failures
                    .push(format!("{}: {error}", entry.id().as_str())),
            }
        }
        Ok(outcome)
    }
}

pub struct ReindexJob {
    store: Arc<dyn ResourceStore>,
}

impl ReindexJob {
    pub fn new(store: Arc<dyn ResourceStore>) -> ReindexJob {
        ReindexJob { store }
    }
}

#[async_trait]
impl JobHandler for ReindexJob {
    fn kind(&self) -> JobKind {
        JobKind::Reindex
    }

    async fn plan(&self, job: &JobContext) -> Result<Vec<Unit>, Error> {
        let parsed = body(&job.payload)?;
        let urls: Vec<String> = parsed
            .get("urls")
            .and_then(Value::as_array)
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let specs = self.specs(&urls).await?;
        Ok(specs
            .into_iter()
            .map(|spec| Unit::new(spec.url.clone(), spec.url))
            .collect())
    }

    async fn process(&self, _job: &JobContext, unit: &Unit) -> Result<UnitOutcome, Error> {
        let specs = self.specs(std::slice::from_ref(&unit.detail)).await?;
        let reports = self.store.reindex(&specs).await?;
        let mut outcome = UnitOutcome::default();
        for report in reports {
            outcome.handled += report.indexed as u64;
            for failure in report.failures {
                outcome
                    .failures
                    .push(format!("{}: {}", failure.resource, failure.reason));
            }
        }
        Ok(outcome)
    }
}

impl ReindexJob {
    async fn specs(&self, urls: &[String]) -> Result<Vec<ParameterSpec>, Error> {
        let resource_type = "SearchParameter".parse::<ResourceType>()?;
        let page = self.store.search(&SearchQuery::of_type(resource_type)).await?;
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
}
