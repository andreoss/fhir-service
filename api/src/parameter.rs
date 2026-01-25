use fhir_core::search::{ParamStatus, ParameterSpec, RegisteredParam};
use fhir_core::{ResourceEnvelope, ResourceType};
use fhir_core::Error;
use fhir_store::{IndexReport, SearchQuery};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::app::AppState;

pub const SEARCH_PARAMETER: &str = "SearchParameter";

pub fn status_of(spec: &ParameterSpec, report: Option<&IndexReport>) -> ParamStatus {
    match (spec.retired, report) {
        (true, Some(report)) if report.backfilled => ParamStatus::PendingDisable,
        (true, _) => ParamStatus::Disabled,
        (false, Some(report)) if report.backfilled => ParamStatus::Searchable,
        (false, _) => ParamStatus::Supported,
    }
}

pub fn entry_of(spec: &ParameterSpec, report: Option<&IndexReport>) -> RegisteredParam {
    RegisteredParam {
        def: Arc::clone(&spec.def),
        base: spec.base.clone(),
        url: spec.url.clone(),
        status: status_of(spec, report),
    }
}

pub fn accepts(state: &AppState, spec: &ParameterSpec) -> Result<(), Error> {
    state.registry.accepts(&entry_of(spec, None))
}

pub async fn install(state: &AppState, spec: &ParameterSpec) -> Result<(), Error> {
    state.registry.register(entry_of(spec, None))?;
    match state.store.index_parameter(spec).await {
        Ok(report) => {
            state.registry.register(entry_of(spec, Some(&report)))?;
            Ok(())
        }
        Err(error) => {
            state.registry.remove(&spec.url);
            Err(error)
        }
    }
}

pub async fn uninstall(state: &AppState, url: &str) -> Result<(), Error> {
    state.registry.remove(url);
    state.store.drop_parameter(url).await
}
pub async fn stored(state: &AppState) -> Result<Vec<(ResourceEnvelope, ParameterSpec)>, Error> {
    let kind: ResourceType = SEARCH_PARAMETER.parse()?;
    let page = state.store.search(&SearchQuery::of_type(kind)).await?;
    Ok(page
        .entries
        .into_iter()
        .filter_map(|envelope| {
            let body: Value = serde_json::from_slice(envelope.raw()).ok()?;
            let spec = ParameterSpec::parse(&body).ok()?;
            Some((envelope, spec))
        })
        .collect())
}

pub fn status_report(state: &AppState, wanted: Option<&str>) -> Result<Vec<u8>, Error> {
    let entries: Vec<RegisteredParam> = state
        .registry
        .entries()
        .into_iter()
        .filter(|entry| wanted.is_none_or(|url| entry.url == url))
        .collect();
    if entries.is_empty() && wanted.is_some() {
        return Err(Error::NotFound);
    }
    let rendered: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let report = state.store.index_report(&entry.url);
            json!({
                "name": "searchParameter",
                "part": [
                    {"name": "url", "valueUri": entry.url},
                    {"name": "code", "valueCode": entry.def.name},
                    {"name": "status", "valueCode": entry.status.as_str()},
                    {"name": "indexed", "valueInteger": report.as_ref().map(|found| found.indexed).unwrap_or_default()},
                    {"name": "values", "valueInteger": report.as_ref().map(|found| found.values).unwrap_or_default()},
                    {"name": "overflow", "valueInteger": report.as_ref().map(|found| found.overflow).unwrap_or_default()},
                    {"name": "failures", "valueInteger": report.as_ref().map(|found| found.failures.len()).unwrap_or_default()}
                ]
            })
        })
        .collect();
    let mut rendered = rendered;
    if wanted.is_none() {
        rendered.extend(
            fhir_core::search::unsupported(state.registry.fhir_version())
                .iter()
                .map(|name| json!({"name": "unsupported", "valueCode": name})),
        );
    }
    let body = json!({"resourceType": "Parameters", "parameter": rendered});
    Ok(serde_json::to_vec(&body).expect("status report is serializable"))
}

pub async fn refresh(state: &AppState) -> Result<u64, Error> {
    let held = stored(state).await?;
    let entries = held
        .iter()
        .map(|(_, spec)| {
            let report = state.store.index_report(&spec.url);
            entry_of(spec, report.as_ref())
        })
        .collect();
    state.registry.replace(entries);
    Ok(state.registry.version())
}

pub async fn reindex(state: &AppState, wanted: Option<&str>) -> Result<Vec<u8>, Error> {
    let held = stored(state).await?;
    let specs: Vec<ParameterSpec> = held
        .into_iter()
        .map(|(_, spec)| spec)
        .filter(|spec| wanted.is_none_or(|url| spec.url == url))
        .collect();
    if specs.is_empty() {
        return match wanted {
            Some(_) => Err(Error::NotFound),
            None => Ok(report_of(&[])),
        };
    }
    let (retired, live): (Vec<ParameterSpec>, Vec<ParameterSpec>) =
        specs.into_iter().partition(|spec| spec.retired);
    for spec in &retired {
        state.store.drop_parameter(&spec.url).await?;
        state.registry.register(entry_of(spec, None))?;
    }
    let reports = state.store.reindex(&live).await?;
    for (spec, report) in live.iter().zip(reports.iter()) {
        state.registry.register(entry_of(spec, Some(report)))?;
    }
    Ok(report_of(&reports))
}

fn report_of(reports: &[IndexReport]) -> Vec<u8> {
    let rendered: Vec<Value> = reports
        .iter()
        .map(|report| {
            let mut parts = vec![
                json!({"name": "url", "valueUri": report.url}),
                json!({"name": "indexed", "valueInteger": report.indexed}),
                json!({"name": "values", "valueInteger": report.values}),
                json!({"name": "overflow", "valueInteger": report.overflow}),
                json!({"name": "failures", "valueInteger": report.failures.len()}),
            ];
            parts.extend(report.failures.iter().map(|failure| {
                json!({
                    "name": "failure",
                    "part": [
                        {"name": "resource", "valueString": failure.resource},
                        {"name": "reason", "valueString": failure.reason}
                    ]
                })
            }));
            json!({"name": "reindexed", "part": parts})
        })
        .collect();
    let body = json!({"resourceType": "Parameters", "parameter": rendered});
    serde_json::to_vec(&body).expect("reindex report is serializable")
}

pub async fn set_status(state: &AppState, url: &str, wanted: ParamStatus) -> Result<(), Error> {
    let retired = match wanted {
        ParamStatus::Disabled | ParamStatus::PendingDisable => true,
        ParamStatus::Supported | ParamStatus::Searchable => false,
    };
    let (envelope, spec) = stored(state)
        .await?
        .into_iter()
        .find(|(_, spec)| spec.url == url)
        .ok_or(Error::NotFound)?;
    if spec.retired != retired {
        let mut body: Value = serde_json::from_slice(envelope.raw())
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
        body["status"] = Value::String(if retired { "retired" } else { "active" }.to_owned());
        let written = ResourceEnvelope::parse(
            envelope.version(),
            &serde_json::to_vec(&body).expect("definition is serializable"),
        )?;
        state
            .store
            .update(written, Some(envelope.version_id()))
            .await?;
    }
    let changed = ParameterSpec {
        retired,
        ..spec.clone()
    };
    let report = state.store.index_report(url);
    state.registry.register(entry_of(&changed, report.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(retired: bool) -> ParameterSpec {
        let status = if retired { "retired" } else { "active" };
        ParameterSpec::parse(&json!({
            "resourceType": "SearchParameter",
            "url": "urn:p:risk-band",
            "status": status,
            "code": "risk-band",
            "base": ["Patient"],
            "type": "token",
            "expression": "Patient.extension.valueCode"
        }))
        .unwrap()
    }

    fn backfilled() -> IndexReport {
        IndexReport {
            backfilled: true,
            ..IndexReport::empty("urn:p:risk-band")
        }
    }

    #[test]
    fn the_status_follows_the_definition_and_the_index() {
        assert_eq!(status_of(&spec(false), None), ParamStatus::Supported);
        assert_eq!(
            status_of(&spec(false), Some(&IndexReport::empty("urn:p:risk-band"))),
            ParamStatus::Supported
        );
        assert_eq!(status_of(&spec(false), Some(&backfilled())), ParamStatus::Searchable);
        assert_eq!(status_of(&spec(true), Some(&backfilled())), ParamStatus::PendingDisable);
        assert_eq!(status_of(&spec(true), None), ParamStatus::Disabled);
    }

    #[test]
    fn an_entry_carries_the_definition_and_its_status() {
        let entry = entry_of(&spec(false), Some(&backfilled()));
        assert_eq!(entry.url, "urn:p:risk-band");
        assert_eq!(entry.status, ParamStatus::Searchable);
        assert_eq!(entry.def.name, "risk-band");
        assert_eq!(entry.base.len(), 1);
    }
}
