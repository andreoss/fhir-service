use async_trait::async_trait;
use fhir_core::{Error, FhirInstant, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::ResourceStore;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

type StoreMap = HashMap<ResourceId, Vec<ResourceEnvelope>>;

pub type Clock = Arc<dyn Fn() -> FhirInstant + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .ok()
            .and_then(|text| FhirInstant::parse(&text).ok())
            .unwrap_or_else(fallback_instant)
    })
}

fn fallback_instant() -> FhirInstant {
    FhirInstant::parse("1970-01-01T00:00:00+00:00").expect("epoch instant is valid")
}

pub struct MemoryStore {
    inner: RwLock<StoreMap>,
    clock: Clock,
}

impl MemoryStore {
    pub fn with_clock(clock: Clock) -> MemoryStore {
        MemoryStore {
            inner: RwLock::new(HashMap::new()),
            clock,
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        MemoryStore::with_clock(system_clock())
    }
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

#[async_trait]
impl ResourceStore for MemoryStore {
    async fn create(&self, envelope: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        if guard.contains_key(envelope.id()) {
            return Err(Error::Duplicate(format!("id {:?} already exists", envelope.id().as_str())));
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

    fn health(&self) -> Result<(), Error> {
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
        if envelope.content_eq(&current) {
            return Ok(current);
        }
        let stored = envelope.stored_with(next_version(current.version_id())?, (self.clock)())?;
        versions.push(stored.clone());
        Ok(stored)
    }
}