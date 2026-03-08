use crate::http;
use fhir_core::{Error, FhirInstant, FhirVersion, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::{HistoryOrder, HistoryQuery, HistoryScope, ResourceStore};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

const PAGE: usize = 200;

fn version_number(version: &VersionId) -> u64 {
    version.as_str().parse::<u64>().unwrap_or_default()
}

fn link_after(links: &Value, relation: &str) -> Option<String> {
    links
        .as_array()?
        .iter()
        .find(|link| link.get("relation").and_then(Value::as_str) == Some(relation))
        .and_then(|link| link.get("url").and_then(Value::as_str).map(str::to_owned))
}

fn path_of(link: &str) -> String {
    match link.split_once("://") {
        Some((_, rest)) => match rest.find('/') {
            Some(at) => rest[at..].to_owned(),
            None => "/".to_owned(),
        },
        None => link.to_owned(),
    }
}

fn etag_version(value: &Value) -> Option<VersionId> {
    let etag = value.pointer("/response/etag").and_then(Value::as_str)?;
    let inner = etag
        .trim()
        .strip_prefix("W/\"")
        .and_then(|held| held.strip_suffix('"'))
        .unwrap_or(etag.trim());
    VersionId::parse(inner).ok()
}

fn last_modified(value: &Value) -> Option<FhirInstant> {
    value
        .pointer("/response/lastModified")
        .and_then(Value::as_str)
        .and_then(|text| FhirInstant::parse(text).ok())
}

fn entry_envelope(
    value: &Value,
    method: &str,
    version: FhirVersion,
) -> Result<ResourceEnvelope, Error> {
    let resource = value.get("resource");
    match method {
        "DELETE" => {
            let url = value
                .pointer("/request/url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (resource_type, id) = url.split_once('/').unwrap_or(("", url));
            let parsed = resource
                .map(serde_json::to_vec)
                .transpose()
                .map_err(|error| Error::InvalidJson(error.to_string()))?
                .map(|bytes| ResourceEnvelope::parse(version, &bytes))
                .transpose()?;
            let version_id = parsed
                .as_ref()
                .map(ResourceEnvelope::version_id)
                .cloned()
                .or_else(|| etag_version(value))
                .ok_or_else(|| Error::InvalidEnvelope("a deletion names no version".to_owned()))?;
            let last_updated = parsed
                .as_ref()
                .map(ResourceEnvelope::last_updated)
                .cloned()
                .or_else(|| last_modified(value))
                .ok_or_else(|| Error::InvalidEnvelope("a deletion names no time".to_owned()))?;
            Ok(ResourceEnvelope::deleted_marker(
                version,
                resource_type.parse()?,
                ResourceId::parse(id)?,
                version_id,
                last_updated,
            ))
        }
        _ => {
            let bytes = serde_json::to_vec(resource.ok_or_else(|| {
                Error::InvalidEnvelope("a history entry carries no resource".to_owned())
            })?)
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
            ResourceEnvelope::parse(version, &bytes)
        }
    }
}

pub fn read_history(address: &str, version: FhirVersion) -> Result<Vec<ResourceEnvelope>, Error> {
    let mut entries = Vec::new();
    let mut next = Some("/_history".to_owned());
    while let Some(path) = next.take() {
        let (status, body) = http::send(address, "localhost", "GET", &path, "")
            .map_err(|error| Error::Internal(format!("the source is not readable: {error}")))?;
        let held: Value =
            serde_json::from_str(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
        if status != 200 {
            return Err(Error::Internal(format!(
                "the source history answered {status}"
            )));
        }
        for item in held
            .get("entry")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let method = item
                .pointer("/request/method")
                .and_then(Value::as_str)
                .unwrap_or("GET");
            entries.push(entry_envelope(item, method, version)?);
        }
        next = link_after(&held["link"], "next").map(|link| path_of(&link));
    }
    Ok(entries)
}

fn sorted(mut entries: Vec<ResourceEnvelope>) -> Vec<ResourceEnvelope> {
    entries.sort_by_key(|envelope| {
        (
            envelope.last_updated().key(),
            version_number(envelope.version_id()),
        )
    });
    entries
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullReport {
    pub read: usize,
    pub written: usize,
    pub skipped: usize,
    pub deleted: usize,
    pub by_type: BTreeMap<String, usize>,
    pub failures: Vec<String>,
    pub millis: u64,
}

impl PullReport {
    pub fn to_value(&self) -> Value {
        json!({
            "read": self.read,
            "written": self.written,
            "skipped": self.skipped,
            "deleted": self.deleted,
            "byType": self.by_type,
            "millis": self.millis,
            "failures": self.failures,
        })
    }
}

pub async fn pull(
    store: &dyn ResourceStore,
    address: &str,
    version: FhirVersion,
) -> Result<PullReport, Error> {
    let started = Instant::now();
    let mut report = PullReport {
        read: 0,
        ..PullReport::default()
    };
    for envelope in sorted(read_history(address, version)?) {
        report.read += 1;
        *report
            .by_type
            .entry(envelope.resource_type().as_str().to_owned())
            .or_default() += 1;
        if envelope.is_deleted() {
            report.deleted += 1;
        }
        let label = format!(
            "{} / {}",
            envelope.resource_type().as_str(),
            envelope.id().as_str()
        );
        match store.restore_version(envelope).await {
            Ok(true) => report.written += 1,
            Ok(false) => report.skipped += 1,
            Err(error) => report.failures.push(format!("{label}: {error}")),
        }
    }
    report.millis = started.elapsed().as_millis() as u64;
    Ok(report)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub source: usize,
    pub target: usize,
    pub kept: usize,
    pub mismatched: Vec<String>,
    pub extra: Vec<String>,
    pub millis: u64,
}

impl ReconcileReport {
    pub fn to_value(&self) -> Value {
        json!({
            "source": self.source,
            "target": self.target,
            "kept": self.kept,
            "mismatched": self.mismatched,
            "extra": self.extra,
            "millis": self.millis,
        })
    }
}

fn key_of(envelope: &ResourceEnvelope) -> (String, String, String) {
    (
        envelope.resource_type().as_str().to_owned(),
        envelope.id().as_str().to_owned(),
        envelope.version_id().as_str().to_owned(),
    )
}

pub async fn reconcile(
    store: &dyn ResourceStore,
    address: &str,
    version: FhirVersion,
) -> Result<ReconcileReport, Error> {
    let started = Instant::now();
    let entries = read_history(address, version)?;
    let expected: HashSet<(String, String, String)> = entries.iter().map(key_of).collect();
    let mut report = ReconcileReport {
        source: entries.len(),
        ..ReconcileReport::default()
    };
    for envelope in &entries {
        let label = format!(
            "{} / {} v{}",
            envelope.resource_type().as_str(),
            envelope.id().as_str(),
            envelope.version_id().as_str()
        );
        match store
            .vread(&fhir_core::ResourceKey::of(envelope), envelope.version_id())
            .await
        {
            Ok(target) => {
                let same = target.is_deleted() == envelope.is_deleted()
                    && target.last_updated() == envelope.last_updated()
                    && target.content_eq(envelope);
                if same {
                    report.kept += 1;
                } else {
                    report.mismatched.push(format!("{label} differs"));
                }
            }
            Err(error) => report.mismatched.push(format!("{label} missing: {error}")),
        }
    }
    let mut offset = 0;
    loop {
        let query = HistoryQuery {
            order: HistoryOrder::Oldest,
            offset,
            count: PAGE,
            ..HistoryQuery::default()
        };
        let page = store.history(&HistoryScope::System, &query).await?;
        if page.total == 0 {
            break;
        }
        let held = page.entries.len();
        for envelope in &page.entries {
            report.target += 1;
            if !expected.contains(&key_of(envelope)) {
                report.extra.push(format!(
                    "{} / {} v{}",
                    envelope.resource_type().as_str(),
                    envelope.id().as_str(),
                    envelope.version_id().as_str()
                ));
            }
        }
        let advance = held.max(1);
        if page.offset + held >= page.total {
            break;
        }
        offset += advance;
    }
    report.millis = started.elapsed().as_millis() as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_next_link_yields_only_the_path() {
        assert_eq!(
            path_of("http://127.0.0.1:9081/_history?since=1&ct=two"),
            "/_history?since=1&ct=two"
        );
        assert_eq!(path_of("/_history?since=1"), "/_history?since=1");
        assert_eq!(path_of("http://127.0.0.1:9081"), "/");
    }

    #[test]
    fn a_history_entry_renders_a_delete_marker_from_its_response() {
        let entry: Value = serde_json::from_str(
            r#"{"request":{"method":"DELETE","url":"Patient/p1"},"response":{"status":"204","etag":"W/\"3\"","lastModified":"2026-09-09T09:11:19.825+00:00"}}"#,
        )
        .unwrap();
        let envelope = entry_envelope(&entry, "DELETE", FhirVersion::R4).unwrap();
        assert!(envelope.is_deleted());
        assert_eq!(envelope.id().as_str(), "p1");
        assert_eq!(envelope.resource_type().as_str(), "Patient");
        assert_eq!(envelope.version_id().as_str(), "3");
        assert_eq!(
            envelope.last_updated().as_str(),
            "2026-09-09T09:11:19.825+00:00"
        );
    }

    #[test]
    fn a_delete_marker_may_carry_its_own_resource() {
        let entry: Value = serde_json::from_str(
            r#"{"request":{"method":"DELETE","url":"Patient/p1"},"resource":{"resourceType":"Patient","id":"p1","meta":{"versionId":"6","lastUpdated":"2026-09-09T09:11:19.825+00:00"}},"response":{"status":"204","etag":"W/\"6\"","lastModified":"2026-09-09T09:11:19.825+00:00"}}"#,
        )
        .unwrap();
        let envelope = entry_envelope(&entry, "DELETE", FhirVersion::R4).unwrap();
        assert!(envelope.is_deleted());
        assert_eq!(envelope.version_id().as_str(), "6");
    }

    #[test]
    fn a_deletion_naming_neither_version_nor_time_is_refused() {
        let entry: Value =
            serde_json::from_str(r#"{"request":{"method":"DELETE","url":"Patient/p1"}}"#).unwrap();
        assert!(entry_envelope(&entry, "DELETE", FhirVersion::R4).is_err());
    }

    #[test]
    fn an_entry_without_a_resource_is_refused_outside_deletions() {
        let entry: Value =
            serde_json::from_str(r#"{"request":{"method":"PUT","url":"Patient/p1"}}"#).unwrap();
        assert!(entry_envelope(&entry, "PUT", FhirVersion::R4).is_err());
    }
}
