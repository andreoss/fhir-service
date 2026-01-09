use fhir_core::search::{ParamStatus, ParameterSpec, RegisteredParam};
use fhir_core::Error;
use fhir_store::IndexReport;
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
