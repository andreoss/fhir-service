







use axum::extract::{Path, State};
use axum::http::header::HeaderMap;
use axum::response::Response;
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, ResourceEnvelope, ResourceId, ResourceKey, ResourceType};
use fhir_store::{SearchQuery, TotalMode};
use serde_json::{json, Map, Value};

use crate::app::AppState;
use crate::handlers::{allowed, AppError};





const LABELS: [&str; 3] = ["profile", "security", "tag"];

const SERVER_OWNED: [&str; 2] = ["versionId", "lastUpdated"];


fn labels_of(body: &Value) -> Map<String, Value> {
    let mut held = Map::new();
    let Some(meta) = body.get("meta") else {
        return held;
    };
    for name in LABELS {
        if let Some(found) = meta.get(name) {
            held.insert(name.to_owned(), found.clone());
        }
    }
    held
}

fn answered(meta: Map<String, Value>) -> Response {
    let body = json!({
        "resourceType": "Parameters",
        "parameter": [{"name": "return", "valueMeta": Value::Object(meta)}],
    });
    crate::handlers::rendered(
        serde_json::to_vec(&body).expect("a parameters resource is serializable"),
    )
}


fn supplied(body: &[u8]) -> Result<Map<String, Value>, Error> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let meta = crate::operation::value_of(&parsed, "meta")
        .map(Value::String)
        .filter(|_| false)
        .or_else(|| named_meta(&parsed))
        .ok_or_else(|| Error::InvalidParameter("the request names no meta".to_owned()))?;
    let object = meta
        .as_object()
        .ok_or_else(|| Error::InvalidParameter("meta is not an object".to_owned()))?;
    for name in SERVER_OWNED {
        if object.contains_key(name) {
            return Err(Error::InvalidParameter(format!(
                "meta.{name} belongs to the server and is not supplied"
            )));
        }
    }
    let mut held = Map::new();
    for name in LABELS {
        if let Some(found) = object.get(name) {
            if !found.is_array() {
                return Err(Error::InvalidParameter(format!("meta.{name} is a list")));
            }
            held.insert(name.to_owned(), found.clone());
        }
    }
    if held.is_empty() {
        return Err(Error::InvalidParameter(
            "the meta names no tag, security label or profile".to_owned(),
        ));
    }
    Ok(held)
}

fn named_meta(parsed: &Value) -> Option<Value> {
    parsed
        .get("parameter")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some("meta"))?
        .get("valueMeta")
        .cloned()
}


fn added(current: &mut Map<String, Value>, supplied: &Map<String, Value>) {
    for (name, values) in supplied {
        let held = current
            .entry(name.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(list) = held.as_array_mut() else {
            continue;
        };
        for value in values.as_array().cloned().unwrap_or_default() {
            if !list.contains(&value) {
                list.push(value);
            }
        }
    }
}

fn removed(current: &mut Map<String, Value>, supplied: &Map<String, Value>) {
    for (name, values) in supplied {
        let Some(held) = current.get_mut(name).and_then(Value::as_array_mut) else {
            continue;
        };
        let going = values.as_array().cloned().unwrap_or_default();
        held.retain(|value| !going.contains(value));
    }
    current.retain(|_, values| !values.as_array().is_some_and(Vec::is_empty));
}

pub async fn read_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let stored = state
        .store
        .read(&ResourceKey::new(resource_type, id))
        .await?;
    if stored.is_deleted() {
        return Err(Error::Deleted.into());
    }
    let body: Value = serde_json::from_slice(stored.raw())
        .map_err(|error| Error::InvalidJson(error.to_string()))?;
    Ok(answered(labels_of(&body)))
}

pub async fn read_type(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
    allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        None,
    )
    .await?;
    Ok(answered(gathered(&state, Some(resource_type)).await?))
}

pub async fn read_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    Ok(answered(gathered(&state, None).await?))
}




async fn gathered(
    state: &AppState,
    resource_type: Option<ResourceType>,
) -> Result<Map<String, Value>, Error> {
    let query = SearchQuery {
        types: resource_type.into_iter().collect(),
        count: usize::MAX,
        total: TotalMode::None,
        ..SearchQuery::default()
    };
    let page = state.store.search(&query).await?;
    let mut held = Map::new();
    for entry in &page.entries {
        let body: Value = serde_json::from_slice(entry.raw())
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
        added(&mut held, &labels_of(&body));
    }
    Ok(held)
}

pub async fn add(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    changed(state, path, headers, &body, true).await
}

pub async fn remove(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    changed(state, path, headers, &body, false).await
}

async fn changed(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
    body: &[u8],
    adding: bool,
) -> Result<Response, AppError> {
    let resource_type = crate::handlers::served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    allowed(
        &state,
        &headers,
        DataAction::Write,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let asked = supplied(body)?;
    let stored = state
        .store
        .read(&ResourceKey::new(resource_type, id.clone()))
        .await?;
    if stored.is_deleted() {
        return Err(Error::Deleted.into());
    }
    let mut value: Value = serde_json::from_slice(stored.raw())
        .map_err(|error| Error::InvalidJson(error.to_string()))?;
    let mut labels = labels_of(&value);
    match adding {
        true => added(&mut labels, &asked),
        false => removed(&mut labels, &asked),
    }
    written(&state, &value, &labels)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::InvalidEnvelope("the stored body is not a resource".to_owned()))?;
    let meta = object
        .entry("meta".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(held) = meta.as_object_mut() {
        for name in LABELS {
            match labels.get(name) {
                Some(found) => {
                    held.insert(name.to_owned(), found.clone());
                }
                None => {
                    held.remove(name);
                }
            }
        }
    }
    let envelope = rewritten(state.version, resource_type, value.clone(), &id)?;
    state.store.update(envelope, None).await?;
    Ok(answered(labels))
}


fn written(state: &AppState, value: &Value, labels: &Map<String, Value>) -> Result<(), Error> {
    let mut candidate = value.clone();
    if let Some(object) = candidate.as_object_mut() {
        object.insert("meta".to_owned(), Value::Object(labels.clone()));
    }
    let findings = fhir_core::Model::of(state.version).check(&candidate);
    match findings.first() {
        None => Ok(()),
        Some(first) => Err(Error::InvalidEnvelope(format!(
            "{} at {}: {}",
            first.rule, first.path, first.detail
        ))),
    }
}

fn rewritten(
    version: fhir_core::FhirVersion,
    resource_type: ResourceType,
    mut value: Value,
    id: &ResourceId,
) -> Result<ResourceEnvelope, Error> {
    if let Some(object) = value.as_object_mut() {
        object.insert("id".to_owned(), Value::String(id.as_str().to_owned()));
    }
    fhir_core::with_assigned_meta(&mut value)?;
    let bytes =
        serde_json::to_vec(&value).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let held = ResourceEnvelope::parse(version, &bytes)?;
    match held.resource_type() == resource_type {
        true => Ok(held),
        false => Err(Error::InvalidEnvelope(
            "the stored resource changed type".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_labels_a_client_owns_are_read() {
        let body = json!({
            "resourceType": "Patient",
            "meta": {
                "versionId": "3",
                "lastUpdated": "2026-01-01T00:00:00Z",
                "tag": [{"system": "urn:s", "code": "a"}],
                "profile": ["http://example.test/p"]
            }
        });
        let held = labels_of(&body);
        assert!(held.contains_key("tag"));
        assert!(held.contains_key("profile"));
        assert!(!held.contains_key("versionId"));
        assert!(!held.contains_key("lastUpdated"));
    }

    #[test]
    fn adding_does_not_repeat_what_is_there() {
        let mut held = labels_of(&json!({"meta": {"tag": [{"code": "a"}]}}));
        added(
            &mut held,
            &labels_of(&json!({"meta": {"tag": [{"code": "a"}, {"code": "b"}]}})),
        );
        assert_eq!(held["tag"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn removing_takes_only_what_matches_and_drops_an_empty_set() {
        let mut held = labels_of(&json!({"meta": {"tag": [{"code": "a"}, {"code": "b"}]}}));
        removed(
            &mut held,
            &labels_of(&json!({"meta": {"tag": [{"code": "a"}]}})),
        );
        assert_eq!(held["tag"].as_array().map(Vec::len), Some(1));
        removed(
            &mut held,
            &labels_of(&json!({"meta": {"tag": [{"code": "b"}]}})),
        );
        assert!(held.get("tag").is_none(), "{held:?}");
    }

    #[test]
    fn a_supplied_meta_is_read_from_the_parameters() {
        let body = json!({
            "resourceType": "Parameters",
            "parameter": [{"name": "meta", "valueMeta": {"tag": [{"code": "a"}]}}]
        })
        .to_string();
        let held = supplied(body.as_bytes()).unwrap();
        assert_eq!(held["tag"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn what_the_server_owns_is_refused_as_input() {
        for name in SERVER_OWNED {
            let body = json!({
                "resourceType": "Parameters",
                "parameter": [{"name": "meta", "valueMeta": {name: "5"}}]
            })
            .to_string();
            let error = supplied(body.as_bytes()).unwrap_err();
            assert!(error.to_string().contains(name), "{error}");
        }
    }

    #[test]
    fn a_request_naming_no_label_is_refused() {
        let body = json!({
            "resourceType": "Parameters",
            "parameter": [{"name": "meta", "valueMeta": {}}]
        })
        .to_string();
        assert!(supplied(body.as_bytes()).is_err());
        assert!(supplied(b"{}").is_err());
    }
}
