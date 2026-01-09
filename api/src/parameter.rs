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

pub async fn restore(state: &AppState, url: &str, stored: Option<&serde_json::Value>) {
    match stored.and_then(|body| ParameterSpec::parse(body).ok()) {
        Some(spec) => {
            let report = state.store.index_report(&spec.url);
            let _ = state.registry.register(entry_of(&spec, report.as_ref()));
        }
        None => {
            let _ = uninstall(state, url).await;
        }
    }
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
    let body = json!({"resourceType": "Parameters", "parameter": rendered});
    Ok(serde_json::to_vec(&body).expect("status report is serializable"))
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
