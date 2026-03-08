








use axum::extract::{Path, State};
use axum::http::header::HeaderMap;
use axum::response::Response;
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, ResourceId, ResourceKey, ResourceType, VersionId};
use fhir_store::{Interaction, SearchQuery};
use serde_json::json;

use crate::app::AppState;
use crate::handlers::{allowed_doing, AppError};




pub const KEPT_ON_PURGE: [&str; 2] = ["AuditEvent", "Provenance"];


const NEVER_ERASED: &str = "AuditEvent";

fn told(removed: usize, what: &[String]) -> Response {
    let body = json!({
        "resourceType": "Parameters",
        "parameter": [
            {"name": "removed", "valueInteger": removed},
            {"name": "resources", "valueString": what.join(", ")},
        ],
    });
    crate::handlers::rendered(
        serde_json::to_vec(&body).expect("a parameters resource is serializable"),
    )
}

pub async fn erase_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    allowed_doing(
        &state,
        &headers,
        DataAction::Write,
        Interaction::Delete,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    refuse_trail(resource_type)?;
    let key = ResourceKey::new(resource_type, id);
    state.store.hard_delete(&key).await?;
    Ok(told(1, &[key.to_string()]))
}

pub async fn erase_version(
    State(state): State<AppState>,
    Path((type_name, id_text, version_text)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let version = version_text.parse::<VersionId>()?;
    allowed_doing(
        &state,
        &headers,
        DataAction::Write,
        Interaction::Delete,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    refuse_trail(resource_type)?;
    let key = ResourceKey::new(resource_type, id);
    let removed = state.store.erase_versions(&key, &version).await?;
    Ok(told(
        removed,
        &[format!("{key}/_history/{version_text} and older")],
    ))
}


pub async fn purge(
    State(state): State<AppState>,
    Path(id_text): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let patient = "Patient".parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    allowed_doing(
        &state,
        &headers,
        DataAction::Write,
        Interaction::Delete,
        Some(patient),
        Some(&id),
    )
    .await?;
    let definition = fhir_core::search::compartment::definition(patient.as_str())
        .ok_or_else(|| Error::UnsupportedParameter("compartment \"Patient\"".to_owned()))?;
    let kept: Vec<ResourceType> = state
        .purge_keeps
        .iter()
        .filter_map(|name| name.parse::<ResourceType>().ok())
        .collect();
    let types: Vec<ResourceType> = definition
        .types()
        .iter()
        .filter_map(|name| name.parse::<ResourceType>().ok())
        .filter(|held| !kept.contains(held))
        .collect();
    let query = SearchQuery {
        types,
        compartment: Some(fhir_core::search::Compartment {
            kind: patient,
            id: id.clone(),
        }),
        count: usize::MAX,
        ..SearchQuery::default()
    };
    let found = state.store.search(&query).await?;
    let mut removed = Vec::new();
    for entry in &found.entries {
        let key = ResourceKey::of(entry);
        if key.resource_type().as_str() == NEVER_ERASED {
            continue;
        }
        state.store.hard_delete(&key).await?;
        removed.push(key.to_string());
    }
    let root = ResourceKey::new(patient, id);
    if state.store.hard_delete(&root).await.is_ok() {
        removed.push(root.to_string());
    }
    Ok(told(removed.len(), &removed))
}

fn refuse_trail(resource_type: ResourceType) -> Result<(), Error> {
    match resource_type.as_str() == NEVER_ERASED {
        true => Err(Error::Forbidden(
            "an AuditEvent is not erased: a trail a caller can erase is not a trail".to_owned(),
        )),
        false => Ok(()),
    }
}
