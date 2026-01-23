use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::convert::{convert, Conversion, InputType};
use fhir_core::validate::{validate, Mode, Request as ValidationRequest};
use fhir_core::{Error, ResourceId, ResourceType};
use serde_json::Value;

use crate::app::AppState;
use crate::query::param;
use crate::handlers::AppError;

const FHIR_JSON: &str = "application/fhir+json";

pub fn value_of(body: &Value, name: &str) -> Option<String> {
    entries(body).find_map(|entry| match entry.get("name").and_then(Value::as_str) {
        Some(held) if held == name => primitive(entry),
        _ => None,
    })
}

pub fn values_of(body: &Value, name: &str) -> Vec<String> {
    entries(body)
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .filter_map(primitive)
        .collect()
}

pub fn resource_of<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    entries(body).find_map(|entry| match entry.get("name").and_then(Value::as_str) {
        Some(held) if held == name => entry.get("resource"),
        _ => None,
    })
}

fn entries(body: &Value) -> impl Iterator<Item = &Value> {
    body.get("parameter")
        .and_then(Value::as_array)
        .map(|items| items.iter())
        .unwrap_or_else(|| [].iter())
}

fn primitive(entry: &Value) -> Option<String> {
    let object = entry.as_object()?;
    object.iter().find_map(|(name, value)| {
        let named = name.starts_with("value") && name != "value";
        match (named, value) {
            (true, Value::String(text)) => Some(text.clone()),
            (true, Value::Number(number)) => Some(number.to_string()),
            (true, Value::Bool(flag)) => Some(flag.to_string()),
            _ => None,
        }
    })
}

pub fn parameters(body: &[u8]) -> Result<Value, Error> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    match value.get("resourceType").and_then(Value::as_str) {
        Some("Parameters") => Ok(value),
        _ => Err(Error::InvalidEnvelope(
            "the operation takes a parameters resource".to_owned(),
        )),
    }
}

fn required(body: &Value, name: &str) -> Result<String, Error> {
    value_of(body, name).ok_or_else(|| Error::InvalidParameter(format!("{name:?} is missing")))
}

pub async fn convert_data(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, AppError> {
    let input = parameters(&body)?;
    let data = required(&input, "inputData")?;
    let input_type = required(&input, "inputDataType")?.parse::<InputType>()?;
    let collection = required(&input, "templateCollectionReference")?;
    let root = required(&input, "rootTemplate")?;
    let converted = convert(
        state.templates.as_ref(),
        &Conversion {
            input_type,
            data: &data,
            collection: &collection,
            root_template: &root,
        },
    )?;
    Ok(rendered(
        serde_json::to_vec(&converted).map_err(|error| Error::Internal(error.to_string()))?,
    ))
}

pub async fn validate_type(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    validated(&state, Some(resource_type), None, query.as_deref(), &body).await
}

pub async fn validate_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    validated(&state, Some(resource_type), Some(id), query.as_deref(), &body).await
}

async fn validated(
    state: &AppState,
    resource_type: Option<ResourceType>,
    id: Option<ResourceId>,
    query: Option<&str>,
    body: &[u8],
) -> Result<Response, AppError> {
    let submitted = submitted(body)?;
    let mut profile = param(query, "profile");
    let mut mode = param(query, "mode");
    let value = match submitted {
        Some(Value::Object(ref object)) if object.get("resourceType") == Some(&Value::String("Parameters".to_owned())) => {
            let held = Value::Object(object.clone());
            profile = profile.or_else(|| value_of(&held, "profile"));
            mode = mode.or_else(|| value_of(&held, "mode"));
            resource_of(&held, "resource").cloned()
        }
        Some(value) => Some(value),
        None => None,
    };
    let mode = match mode {
        Some(text) => text.parse::<Mode>()?,
        None => Mode::Update,
    };
    let body = match (value, &id) {
        (Some(value), _) => value,
        (None, Some(id)) => {
            let stored = state.store.read(id).await?;
            if stored.is_deleted() {
                return Err(Error::Deleted.into());
            }
            serde_json::from_slice(stored.raw())
                .map_err(|error| Error::InvalidJson(error.to_string()))?
        }
        (None, None) if mode == Mode::Delete => Value::Null,
        (None, None) => {
            return Err(Error::InvalidParameter("no resource to validate".to_owned()).into())
        }
    };
    let report = validate(&ValidationRequest {
        resource_type,
        id,
        profile: profile.as_deref(),
        mode,
        body: &body,
    });
    Ok(rendered(report.to_fhir_json()))
}

fn submitted(body: &[u8]) -> Result<Option<Value>, Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(Some)
        .map_err(|error| Error::InvalidJson(error.to_string()))
}

pub(crate) fn rendered(body: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_are_read_by_name() {
        let body = serde_json::json!({
            "resourceType": "Parameters",
            "parameter": [
                {"name": "url", "valueUri": "urn:x"},
                {"name": "count", "valueInteger": 5},
                {"name": "active", "valueBoolean": true},
                {"name": "code", "valueCode": "a"},
                {"name": "code", "valueCode": "b"},
                {"name": "patient", "resource": {"resourceType": "Patient"}},
                {"name": "empty"}
            ]
        });
        assert_eq!(value_of(&body, "url"), Some("urn:x".to_owned()));
        assert_eq!(value_of(&body, "count"), Some("5".to_owned()));
        assert_eq!(value_of(&body, "active"), Some("true".to_owned()));
        assert_eq!(values_of(&body, "code"), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(value_of(&body, "empty"), None);
        assert_eq!(value_of(&body, "nonesuch"), None);
        assert_eq!(
            resource_of(&body, "patient").and_then(|found| found["resourceType"].as_str()),
            Some("Patient")
        );
        assert!(resource_of(&body, "url").is_none());
        assert!(values_of(&serde_json::json!({}), "code").is_empty());
    }

    #[test]
    fn a_body_that_is_not_a_parameters_resource_is_refused() {
        assert!(matches!(
            parameters(b"not json").unwrap_err(),
            Error::InvalidJson(_)
        ));
        assert!(matches!(
            parameters(br#"{"resourceType":"Patient"}"#).unwrap_err(),
            Error::InvalidEnvelope(_)
        ));
        let body = parameters(br#"{"resourceType":"Parameters"}"#).unwrap();
        assert!(matches!(
            required(&body, "url").unwrap_err(),
            Error::InvalidParameter(_)
        ));
    }
}
