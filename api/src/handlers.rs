use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::{Error, FhirInstant, ResourceEnvelope, ResourceId, ResourceType, VersionId, WeakEtag};
use fhir_store::SearchParams;
use serde_json::Value;
use uuid::Uuid;

use crate::app::AppState;
use crate::query::conditional_params;

const FHIR_JSON: &str = "application/fhir+json";
const IF_NONE_EXIST: &str = "if-none-exist";
const PLACEHOLDER_VERSION: &str = "0";
const PLACEHOLDER_INSTANT: &str = "1970-01-01T00:00:00Z";

pub struct AppError(Error);

impl From<Error> for AppError {
    fn from(error: Error) -> AppError {
        AppError(error)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let outcome = self.0.to_operation_outcome();
        let status = StatusCode::from_u16(outcome.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            [(header::CONTENT_TYPE, FHIR_JSON), (header::CACHE_CONTROL, "no-store")],
            outcome.to_fhir_json(),
        )
            .into_response()
    }
}

pub async fn read(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    let envelope = state.store.read(&id).await?;
    if envelope.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    Ok(respond_resource(&envelope, host_from(&headers)))
}

pub async fn vread(
    State(state): State<AppState>,
    Path((type_name, id_text, version_text)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    let version = version_text.parse::<VersionId>()?;
    let envelope = state.store.vread(&id, &version).await?;
    if envelope.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    Ok(respond_resource(&envelope, host_from(&headers)))
}

pub async fn create(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    if let Some(condition) = headers.get(IF_NONE_EXIST) {
        let raw = condition
            .to_str()
            .map_err(|_| Error::InvalidEnvelope("if-none-exist is not ascii".to_owned()))?;
        let params = require_condition(conditional_params(Some(raw)), "if-none-exist")?;
        if let Some(existing) = single_match(&state, resource_type, &params).await? {
            return Ok(respond_updated(&existing, host_from(&headers)));
        }
    }
    let value: Value = serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let id = body_id(&value)?;
    let envelope = write_envelope(state.version, resource_type, value, &id)?;
    let stored = state.store.create(envelope).await?;
    Ok(respond_created(&stored, host_from(&headers)))
}

pub async fn conditional_update(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let params = require_condition(conditional_params(query.as_deref()), "conditional update")?;
    let value: Value = serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let expected = expected_version(&headers)?;
    match single_match(&state, resource_type, &params).await? {
        Some(existing) => {
            let envelope = write_envelope(state.version, resource_type, value, existing.id())?;
            let stored = state.store.update(envelope, expected.as_ref()).await?;
            Ok(respond_updated(&stored, host_from(&headers)))
        }
        None => {
            let id = body_id(&value)?;
            let envelope = write_envelope(state.version, resource_type, value, &id)?;
            let stored = state.store.create(envelope).await?;
            Ok(respond_created(&stored, host_from(&headers)))
        }
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let id = id_text.parse::<ResourceId>()?;
    let expected = expected_version(&headers)?;
    let value: Value = serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let envelope = write_envelope(state.version, resource_type, value, &id)?;
    let stored = state.store.update(envelope, expected.as_ref()).await?;
    Ok(respond_updated(&stored, host_from(&headers)))
}

pub async fn health(State(state): State<AppState>) -> Response {
    let mut any_failure = false;
    let dependencies: Vec<Value> = state
        .dependencies
        .iter()
        .map(|dependency| match (dependency.check)() {
            Ok(()) => serde_json::json!({ "name": dependency.name, "status": "ok" }),
            Err(message) => {
                any_failure = true;
                serde_json::json!({ "name": dependency.name, "status": "error", "detail": message })
            }
        })
        .collect();
    let status = if any_failure { "degraded" } else { "ok" };
    let status_code = if any_failure { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::OK };
    let body = serde_json::to_vec(&serde_json::json!({ "status": status, "dependencies": dependencies }))
        .expect("health payload is serializable");
    (
        status_code,
        [(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")],
        body,
    )
        .into_response()
}

pub async fn not_found() -> Result<Response, AppError> {
    Err(Error::NotFound.into())
}

pub async fn method_not_allowed() -> Result<Response, AppError> {
    Err(Error::MethodNotAllowed.into())
}

fn require_condition(params: SearchParams, what: &str) -> Result<SearchParams, Error> {
    if params.is_empty() {
        return Err(Error::InvalidEnvelope(format!("{what} requires search parameters")));
    }
    Ok(params)
}

async fn single_match(
    state: &AppState,
    resource_type: ResourceType,
    params: &SearchParams,
) -> Result<Option<ResourceEnvelope>, Error> {
    let mut matches = state.store.search(Some(resource_type), params).await?;
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.remove(0))),
        _ => Err(Error::MultipleMatches),
    }
}

fn body_id(value: &Value) -> Result<ResourceId, Error> {
    match value.get("id") {
        Some(Value::String(text)) => text.parse::<ResourceId>(),
        Some(_) => Err(Error::InvalidEnvelope("id must be a string".to_owned())),
        None => ResourceId::parse(&Uuid::new_v4().to_string()),
    }
}

fn expected_version(headers: &HeaderMap) -> Result<Option<VersionId>, Error> {
    match headers.get(header::IF_MATCH) {
        Some(value) => {
            let text = value.to_str().map_err(|_| Error::InvalidEtag("if-match is not ascii".to_owned()))?;
            Ok(Some(WeakEtag::try_from(text)?.as_str().parse::<VersionId>()?))
        }
        None => Ok(None),
    }
}

fn respond_resource(envelope: &ResourceEnvelope, host: &str) -> Response {
    let mut response = Response::new(Body::from(envelope.raw().to_vec()));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
    headers.insert(header::ETAG, HeaderValue::from_str(&etag(envelope)).expect("etag is a header value"));
    headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&last_modified(envelope.last_updated())).expect("last-modified is a header value"),
    );
    headers.insert(
        header::CONTENT_LOCATION,
        HeaderValue::from_str(&location(host, envelope)).expect("content-location is a header value"),
    );
    response
}

fn respond_created(envelope: &ResourceEnvelope, host: &str) -> Response {
    let mut response = respond_resource(envelope, host);
    *response.status_mut() = StatusCode::CREATED;
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_str(&location(host, envelope)).expect("location is a header value"));
    response
}

fn respond_updated(envelope: &ResourceEnvelope, host: &str) -> Response {
    let mut response = respond_resource(envelope, host);
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_str(&location(host, envelope)).expect("location is a header value"));
    response
}

fn etag(envelope: &ResourceEnvelope) -> String {
    WeakEtag::from(envelope.version_id()).to_string()
}

fn host_from(headers: &HeaderMap) -> &str {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost")
}

fn location(host: &str, envelope: &ResourceEnvelope) -> String {
    let host = if host.trim().is_empty() { "localhost" } else { host.trim() };
    format!(
        "http://{host}/{}/{}/_history/{}",
        envelope.resource_type(),
        envelope.id(),
        envelope.version_id()
    )
}

fn last_modified(instant: &FhirInstant) -> String {
    match time::OffsetDateTime::parse(instant.as_str(), &time::format_description::well_known::Rfc3339) {
        Ok(parsed) => {
            let utc = parsed.to_offset(time::UtcOffset::UTC);
            match utc.format(&time::format_description::well_known::Rfc2822) {
                Ok(text) => text
                    .strip_suffix(" +0000")
                    .map(|head| format!("{head} GMT"))
                    .unwrap_or(text),
                Err(_) => instant.as_str().to_owned(),
            }
        }
        Err(_) => instant.as_str().to_owned(),
    }
}

fn write_envelope(
    version: fhir_core::FhirVersion,
    path_type: ResourceType,
    mut value: Value,
    id: &ResourceId,
) -> Result<ResourceEnvelope, Error> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::InvalidEnvelope("expected a JSON object".to_owned()))?;
    let body_type = object
        .get("resourceType")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidEnvelope("missing or non-string resourceType".to_owned()))?;
    if body_type.parse::<ResourceType>()? != path_type {
        return Err(Error::InvalidEnvelope("resource type does not match the request path".to_owned()));
    }
    let id_state = match object.get("id") {
        None => None,
        Some(Value::String(text)) if text == id.as_str() => Some(true),
        Some(Value::String(_)) => Some(false),
        Some(_) => return Err(Error::InvalidEnvelope("id must be a string".to_owned())),
    };
    match id_state {
        None => {
            object.insert("id".to_owned(), Value::String(id.as_str().to_owned()));
        }
        Some(false) => return Err(Error::InvalidEnvelope("id does not match the request path".to_owned())),
        Some(true) => {}
    }
    if !object.contains_key("meta") {
        object.insert("meta".to_owned(), Value::Object(serde_json::Map::new()));
    }
    let meta = object
        .get_mut("meta")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| Error::InvalidEnvelope("meta must be an object".to_owned()))?;
    meta.entry("versionId".to_owned())
        .or_insert_with(|| Value::String(PLACEHOLDER_VERSION.to_owned()));
    meta.entry("lastUpdated".to_owned())
        .or_insert_with(|| Value::String(PLACEHOLDER_INSTANT.to_owned()));
    let bytes = serde_json::to_vec(&value).map_err(|error| Error::InvalidJson(error.to_string()))?;
    ResourceEnvelope::parse(version, &bytes)
}