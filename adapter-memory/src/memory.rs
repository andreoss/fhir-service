use async_trait::async_trait;
use fhir_core::{Error, FhirInstant, ResourceEnvelope, ResourceId, ResourceType, VersionId};
use fhir_store::{ResourceStore, SearchParams};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

type StoreMap = HashMap<ResourceId, Vec<ResourceEnvelope>>;

fn matches_params(envelope: &ResourceEnvelope, params: &SearchParams) -> Result<bool, Error> {
    let text = std::str::from_utf8(envelope.raw()).map_err(|e| Error::InvalidJson(e.to_string()))?;
    let value: Value = serde_json::from_str(text).map_err(|e| Error::InvalidJson(e.to_string()))?;
    for (name, expected) in params {
        let matched = match name.as_str() {
            "_id" => envelope.id().as_str() == expected.as_str(),
            other => match value.get(other) {
                Some(field) => field_matches(field, expected),
                None => false,
            },
        };
        if !matched {
            return Ok(false);
        }
    }
    Ok(true)
}

fn field_matches(field: &Value, expected: &str) -> bool {
    match field {
        Value::Array(items) => items.iter().any(|item| field_matches(item, expected)),
        Value::String(text) => text.eq_ignore_ascii_case(expected),
        Value::Bool(value) => expected.eq_ignore_ascii_case(&value.to_string()),
        Value::Number(number) => match (number.as_f64(), expected.parse::<f64>()) {
            (Some(actual), Ok(wanted)) => (actual - wanted).abs() < f64::EPSILON,
            _ => false,
        },
        Value::Object(map) => {
            let code = expected.rsplit('|').next().unwrap_or_default();
            !code.is_empty()
                && ["value", "code", "text", "reference", "system"].iter().any(|key| {
                    matches!(map.get(*key), Some(Value::String(text)) if text.eq_ignore_ascii_case(code))
                })
        }
        Value::Null => false,
    }
}

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

    async fn search(
        &self,
        resource_type: Option<ResourceType>,
        params: &SearchParams,
    ) -> Result<Vec<ResourceEnvelope>, Error> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let mut matches = Vec::new();
        for versions in guard.values() {
            let Some(current) = versions.last() else { continue };
            if resource_type.is_some_and(|wanted| current.resource_type() != wanted) {
                continue;
            }
            if matches_params(current, params)? {
                matches.push(current.clone());
            }
        }
        matches.sort_by(|a, b| a.id().as_str().cmp(b.id().as_str()));
        Ok(matches)
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