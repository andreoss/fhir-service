use fhir_core::{Error, FhirVersion, InstantPeriod, ResourceEnvelope, WeakEtag};
use fhir_store::{HistoryOrder, HistoryPage, HistoryQuery};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::query::param;
use crate::token::{decode, encode, scope, scope_of, with_token};

const DEFAULT_COUNT: usize = 20;
const MAX_COUNT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summary {
    Full,
    Metadata,
    Text,
    Count,
}

#[derive(Debug, Clone)]
pub struct HistoryRequest {
    pub query: HistoryQuery,
    pub summary: Summary,
}

impl HistoryRequest {
    pub fn parse(raw: Option<&str>) -> Result<HistoryRequest, Error> {
        let summary = match param(raw, "_summary").as_deref() {
            None | Some("false") | Some("data") => Summary::Full,
            Some("true") => Summary::Metadata,
            Some("text") => Summary::Text,
            Some("count") => Summary::Count,
            Some(other) => {
                return Err(Error::UnsupportedParameter(format!("_summary {other:?}")))
            }
        };
        let order = match param(raw, "_sort").as_deref() {
            None | Some("-_lastUpdated") => HistoryOrder::Newest,
            Some("_lastUpdated") => HistoryOrder::Oldest,
            Some(other) => return Err(Error::UnsupportedParameter(format!("_sort {other:?}"))),
        };
        let requested = match param(raw, "_count") {
            Some(text) => text
                .parse::<usize>()
                .map_err(|_| Error::InvalidParameter(format!("_count {text:?}")))?
                .min(MAX_COUNT),
            None => DEFAULT_COUNT,
        };
        let count = match summary {
            Summary::Count => 0,
            _ => requested,
        };
        let offset = match param(raw, "ct") {
            Some(text) => decode(&text, &scope(raw))?,
            None => 0,
        };
        Ok(HistoryRequest {
            query: HistoryQuery {
                since: period(raw, "_since")?,
                at: period(raw, "_at")?,
                before: period(raw, "_before")?,
                order,
                offset,
                count,
            },
            summary,
        })
    }
}

pub fn history_bundle(
    base: &str,
    self_url: &str,
    page: &HistoryPage,
    summary: Summary,
    version: FhirVersion,
) -> Vec<u8> {
    let mut links = vec![serde_json::json!({ "relation": "self", "url": self_url })];
    let consumed = page.offset + page.entries.len();
    if consumed < page.total && !page.entries.is_empty() {
        links.push(serde_json::json!({
            "relation": "next",
            "url": with_token(self_url, &encode(consumed, &scope_of(self_url))),
        }));
    }
    let mut bundle = Map::new();
    bundle.insert("resourceType".to_owned(), Value::String("Bundle".to_owned()));
    bundle.insert("id".to_owned(), Value::String(Uuid::new_v4().to_string()));
    bundle.insert("type".to_owned(), Value::String("history".to_owned()));
    bundle.insert("total".to_owned(), Value::from(page.total));
    bundle.insert("link".to_owned(), Value::Array(links));
    if !page.entries.is_empty() {
        let entries: Vec<Value> = page
            .entries
            .iter()
            .map(|envelope| entry(base, envelope, summary, version))
            .collect();
        bundle.insert("entry".to_owned(), Value::Array(entries));
    }
    serde_json::to_vec(&Value::Object(bundle)).expect("history bundle is serializable")
}

fn period(raw: Option<&str>, name: &str) -> Result<Option<InstantPeriod>, Error> {
    match param(raw, name) {
        Some(text) => Ok(Some(InstantPeriod::parse(&text)?)),
        None => Ok(None),
    }
}

fn entry(
    base: &str,
    envelope: &ResourceEnvelope,
    summary: Summary,
    version: FhirVersion,
) -> Value {
    let resource_type = envelope.resource_type().as_str().to_owned();
    let id = envelope.id().as_str().to_owned();
    let (method, url, status) = if envelope.is_deleted() {
        ("DELETE", format!("{resource_type}/{id}"), "204")
    } else if envelope.version_id().as_str() == "1" {
        ("POST", resource_type.clone(), "201")
    } else {
        ("PUT", format!("{resource_type}/{id}"), "200")
    };
    let mut entry = Map::new();
    entry.insert("fullUrl".to_owned(), Value::String(format!("{base}/{resource_type}/{id}")));
    if let Some(resource) = resource_of(envelope, summary) {
        entry.insert("resource".to_owned(), resource);
    }
    entry.insert("request".to_owned(), serde_json::json!({ "method": method, "url": url }));
    if !matches!(version, FhirVersion::Stu3) {
        entry.insert(
            "response".to_owned(),
            serde_json::json!({
                "status": status,
                "etag": WeakEtag::from(envelope.version_id()).to_string(),
                "lastModified": envelope.last_updated().as_str(),
            }),
        );
    }
    Value::Object(entry)
}

fn resource_of(envelope: &ResourceEnvelope, summary: Summary) -> Option<Value> {
    if envelope.is_deleted() {
        return None;
    }
    match summary {
        Summary::Count => None,
        Summary::Metadata => serde_json::from_slice(&envelope.to_metadata_json()).ok(),
        Summary::Text => crate::search::narrowed(envelope, &["text".to_owned()]),
        Summary::Full => serde_json::from_slice(envelope.raw()).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_nothing_is_asked_for() {
        let request = HistoryRequest::parse(None).unwrap();
        assert_eq!(request.summary, Summary::Full);
        assert_eq!(request.query.order, HistoryOrder::Newest);
        assert_eq!(request.query.count, DEFAULT_COUNT);
        assert_eq!(request.query.offset, 0);
        assert!(request.query.since.is_none());
    }

    #[test]
    fn count_is_capped_and_summary_count_drops_entries() {
        assert_eq!(HistoryRequest::parse(Some("_count=5000")).unwrap().query.count, MAX_COUNT);
        assert_eq!(HistoryRequest::parse(Some("_summary=count")).unwrap().query.count, 0);
    }

}
