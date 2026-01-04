use async_trait::async_trait;
use fhir_core::{Error, FhirInstant, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::{
    HistoryOrder, HistoryPage, HistoryQuery, HistoryScope, ResourceStore, SearchPage, SearchQuery,
    SortDirection, SortKey, TotalMode,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

type StoreMap = HashMap<ResourceId, Vec<ResourceEnvelope>>;

fn body_of(envelope: &ResourceEnvelope) -> Result<Value, Error> {
    serde_json::from_slice(envelope.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

fn reference_of(envelope: &ResourceEnvelope) -> String {
    format!("{}/{}", envelope.resource_type().as_str(), envelope.id().as_str())
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

fn order(matches: &mut [(ResourceEnvelope, Value)], keys: &[SortKey]) {
    matches.sort_by(|left, right| {
        for key in keys {
            let a = fhir_core::search::sort_value(key.target, left.0.id(), left.0.last_updated(), &left.1);
            let b = fhir_core::search::sort_value(key.target, right.0.id(), right.0.last_updated(), &right.1);
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

fn version_number(envelope: &ResourceEnvelope) -> u64 {
    envelope.version_id().as_str().parse::<u64>().unwrap_or_default()
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

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, Error> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::Internal("store lock poisoned".to_owned()))?;
        let members = match &query.list {
            Some(id) => Some(list_members(&guard, id)?),
            None => None,
        };
        let mut matches: Vec<(ResourceEnvelope, Value)> = Vec::new();
        for versions in guard.values() {
            let Some(current) = versions.last() else { continue };
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
            let kept = query
                .filters
                .iter()
                .all(|filter| filter.matches(current.id(), current.last_updated(), &body));
            if kept {
                matches.push((current.clone(), body));
            }
        }
        order(&mut matches, &query.sort);
        let total = match query.total {
            TotalMode::None => None,
            TotalMode::Accurate | TotalMode::Estimate => Some(matches.len()),
        };
        let entries = matches
            .into_iter()
            .skip(query.offset)
            .take(query.count)
            .map(|(envelope, _)| envelope)
            .collect();
        Ok(SearchPage {
            entries,
            total,
            offset: query.offset,
        })
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
                if versions.first().is_none_or(|first| first.resource_type() != *resource_type) {
                    return Err(Error::NotFound);
                }
                versions.iter().filter(|entry| query.keeps(entry)).cloned().collect()
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
        let entries = matches.into_iter().skip(query.offset).take(query.count).collect();
        Ok(HistoryPage {
            entries,
            total,
            offset: query.offset,
        })
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
        if !current.is_deleted() && envelope.content_eq(&current) {
            return Ok(current);
        }
        let stored = envelope.stored_with(next_version(current.version_id())?, (self.clock)())?;
        versions.push(stored.clone());
        Ok(stored)
    }

    async fn delete(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
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
