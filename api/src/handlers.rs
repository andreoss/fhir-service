use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::{
    Error, FhirInstant, Patch, ResourceEnvelope, ResourceId, ResourceType, VersionId, WeakEtag,
};
use fhir_core::search::{Compartment, Grant, ParameterSpec};
use fhir_core::security::scope::DataAction;
use fhir_core::security::Access;
use fhir_store::{HistoryScope, SearchQuery};
use serde_json::Value;
use uuid::Uuid;

use crate::app::AppState;
use crate::history::{history_bundle, HistoryRequest};
use crate::query::param;
use crate::compartment::{definition_json, definitions_bundle};
use crate::parameter::{self, SEARCH_PARAMETER};
use crate::search::{parse_query, search_bundle, SearchRequest};

const FHIR_JSON: &str = "application/fhir+json";
const IF_NONE_EXIST: &str = "if-none-exist";
const HARD_DELETE: &str = "_hardDelete";
const SCOPE: &str = "x-scope";

pub struct AppError(Error);

impl From<Error> for AppError {
    fn from(error: Error) -> AppError {
        AppError(error)
    }
}

impl AppError {
    pub fn into_response_now(self) -> Response {
        self.into_response()
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
    let access = allowed(&state, &headers, DataAction::Read, Some(resource_type)).await?;
    let id = id_text.parse::<ResourceId>()?;
    let envelope = state.store.read(&id).await?;
    if envelope.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    if envelope.is_deleted() {
        return Err(Error::Deleted.into());
    }
    within(&state, &access, &headers, DataAction::Read, &envelope)?;
    Ok(respond_resource(&envelope, host_from(&headers)))
}

pub async fn vread(
    State(state): State<AppState>,
    Path((type_name, id_text, version_text)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Read, Some(resource_type)).await?;
    let id = id_text.parse::<ResourceId>()?;
    let version = version_text.parse::<VersionId>()?;
    let envelope = state.store.vread(&id, &version).await?;
    if envelope.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    if envelope.is_deleted() {
        return Err(Error::Deleted.into());
    }
    within(&state, &access, &headers, DataAction::Read, &envelope)?;
    Ok(respond_resource(&envelope, host_from(&headers)))
}

pub async fn create(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    if let Some(condition) = headers.get(IF_NONE_EXIST) {
        let raw = condition
            .to_str()
            .map_err(|_| Error::InvalidEnvelope("if-none-exist is not ascii".to_owned()))?;
        let query = require_condition(parse_query(&state.registry, Some(resource_type), Some(raw))?, "if-none-exist")?;
        if let Some(existing) = single_match(&state, &query).await? {
            return Ok(respond_updated(&existing, host_from(&headers)));
        }
    }
    let value: Value = serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let id = body_id(&value)?;
    let envelope = write_envelope(state.version, resource_type, value.clone(), &id)?;
    within(&state, &access, &headers, DataAction::Write, &envelope)?;
    if resource_type.as_str() == SEARCH_PARAMETER {
        let spec = ParameterSpec::parse(&value)?;
        let _guard = state.parameters.lock().await;
        parameter::accepts(&state, &spec)?;
        let stored = state.store.create(envelope).await?;
        parameter::install(&state, &spec).await?;
        return Ok(respond_created(&stored, host_from(&headers)));
    }
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
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    let mut selection = require_condition(parse_query(&state.registry, Some(resource_type), query.as_deref())?, "conditional update")?;
    confine(&mut selection, confining(&state, &access, &headers, DataAction::Write)?)?;
    let value: Value = serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let expected = expected_version(&headers)?;
    match single_match(&state, &selection).await? {
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
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    let id = id_text.parse::<ResourceId>()?;
    let expected = expected_version(&headers)?;
    let value: Value = serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let envelope = write_envelope(state.version, resource_type, value.clone(), &id)?;
    within(&state, &access, &headers, DataAction::Write, &envelope)?;
    if resource_type.as_str() == SEARCH_PARAMETER {
        return replace_parameter(&state, &id, &value, envelope, expected, &headers).await;
    }
    let stored = state.store.update(envelope, expected.as_ref()).await?;
    Ok(respond_updated(&stored, host_from(&headers)))
}

async fn replace_parameter(
    state: &AppState,
    id: &ResourceId,
    value: &Value,
    envelope: ResourceEnvelope,
    expected: Option<VersionId>,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let spec = ParameterSpec::parse(value)?;
    let _guard = state.parameters.lock().await;
    parameter::accepts(state, &spec)?;
    let previous = state.store.read(id).await.ok();
    let stored = state.store.update(envelope, expected.as_ref()).await?;
    let replaced = previous
        .as_ref()
        .and_then(|found| serde_json::from_slice::<Value>(found.raw()).ok())
        .and_then(|body| ParameterSpec::parse(&body).ok());
    if let Some(replaced) = replaced {
        if replaced.url != spec.url {
            parameter::uninstall(state, &replaced.url).await?;
        }
    }
    parameter::install(state, &spec).await?;
    Ok(respond_updated(&stored, host_from(headers)))
}

pub async fn delete_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    let id = id_text.parse::<ResourceId>()?;
    let current = state.store.read(&id).await?;
    if current.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    within(&state, &access, &headers, DataAction::Write, &current)?;
    let removed = remove(&state, &id, hard_delete(query.as_deref())).await?;
    if resource_type.as_str() == SEARCH_PARAMETER {
        if let Ok(body) = serde_json::from_slice::<Value>(current.raw()) {
            if let Ok(spec) = ParameterSpec::parse(&body) {
                parameter::uninstall(&state, &spec.url).await?;
            }
        }
    }
    Ok(removed)
}

pub async fn conditional_delete(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    let mut selection = require_condition(parse_query(&state.registry, Some(resource_type), query.as_deref())?, "conditional delete")?;
    confine(&mut selection, confining(&state, &access, &headers, DataAction::Write)?)?;
    match single_match(&state, &selection).await? {
        Some(existing) => remove(&state, existing.id(), hard_delete(query.as_deref())).await,
        None => Err(Error::NotFound.into()),
    }
}

pub async fn patch_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    let id = id_text.parse::<ResourceId>()?;
    let current = state.store.read(&id).await?;
    if current.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    within(&state, &access, &headers, DataAction::Write, &current)?;
    if current.is_deleted() {
        return Err(Error::Deleted.into());
    }
    patch_stored(&state, resource_type, &current, &headers, &body).await
}

pub async fn conditional_patch(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    let mut selection = require_condition(parse_query(&state.registry, Some(resource_type), query.as_deref())?, "conditional patch")?;
    confine(&mut selection, confining(&state, &access, &headers, DataAction::Write)?)?;
    match single_match(&state, &selection).await? {
        Some(existing) => patch_stored(&state, resource_type, &existing, &headers, &body).await,
        None => Err(Error::NotFound.into()),
    }
}

pub async fn purge_history(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Write, Some(resource_type)).await?;
    let id = id_text.parse::<ResourceId>()?;
    let current = state.store.read(&id).await?;
    if current.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    within(&state, &access, &headers, DataAction::Write, &current)?;
    let purged = state.store.purge_history(&id).await?;
    let body = serde_json::json!({
        "resourceType": "Parameters",
        "parameter": [{ "name": "versionsPurged", "valueInteger": purged }],
    });
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, FHIR_JSON)],
        serde_json::to_vec(&body).expect("parameters payload is serializable"),
    )
        .into_response())
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

async fn patch_stored(
    state: &AppState,
    resource_type: ResourceType,
    current: &ResourceEnvelope,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response, AppError> {
    let patched = Patch::parse(body)?.apply(current.raw())?;
    let value: Value = serde_json::from_slice(&patched).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let envelope = write_envelope(state.version, resource_type, value, current.id())?;
    let expected = expected_version(headers)?;
    let stored = state.store.update(envelope, expected.as_ref()).await?;
    Ok(respond_updated(&stored, host_from(headers)))
}

async fn remove(state: &AppState, id: &ResourceId, hard: bool) -> Result<Response, AppError> {
    if hard {
        state.store.hard_delete(id).await?;
        return Ok(no_content(None));
    }
    match state.store.delete(id).await {
        Ok(marker) => Ok(no_content(Some(&marker))),
        Err(Error::Deleted) => Ok(no_content(None)),
        Err(error) => Err(error.into()),
    }
}

fn hard_delete(query: Option<&str>) -> bool {
    param(query, HARD_DELETE).is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn no_content(marker: Option<&ResourceEnvelope>) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    if let Some(marker) = marker {
        response
            .headers_mut()
            .insert(header::ETAG, HeaderValue::from_str(&etag(marker)).expect("etag is a header value"));
    }
    response
}

fn require_condition(query: SearchQuery, what: &str) -> Result<SearchQuery, Error> {
    if query.is_unconditional() {
        return Err(Error::InvalidEnvelope(format!("{what} requires search parameters")));
    }
    Ok(query)
}

async fn single_match(
    state: &AppState,
    query: &SearchQuery,
) -> Result<Option<ResourceEnvelope>, Error> {
    let mut page = state.store.search(query).await?;
    match page.entries.len() {
        0 => Ok(None),
        1 => Ok(Some(page.entries.remove(0))),
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

pub(crate) fn host_from(headers: &HeaderMap) -> &str {
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
    fhir_core::with_assigned_meta(&mut value)?;
    let bytes = serde_json::to_vec(&value).map_err(|error| Error::InvalidJson(error.to_string()))?;
    ResourceEnvelope::parse(version, &bytes)
}
pub async fn system_history(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None).await?;
    respond_history(&state, HistoryScope::System, "/_history".to_owned(), query, &headers).await
}

pub async fn type_history(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    allowed(&state, &headers, DataAction::Read, Some(resource_type)).await?;
    let path = format!("/{resource_type}/_history");
    respond_history(&state, HistoryScope::Type(resource_type), path, query, &headers).await
}

pub async fn instance_history(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    let access = allowed(&state, &headers, DataAction::Read, Some(resource_type)).await?;
    let id = id_text.parse::<ResourceId>()?;
    let path = format!("/{resource_type}/{id}/_history");
    let current = state.store.read(&id).await?;
    within(&state, &access, &headers, DataAction::Read, &current)?;
    let scope = HistoryScope::Instance(resource_type, id);
    respond_history(&state, scope, path, query, &headers).await
}

async fn respond_history(
    state: &AppState,
    scope: HistoryScope,
    path: String,
    query: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let request = HistoryRequest::parse(query.as_deref())?;
    let access = crate::access::access_of(state, headers).await?;
    covers(&confining(state, &access, headers, DataAction::Read)?, &scope)?;
    let page = state.store.history(&scope, &request.query).await?;
    let base = format!("http://{}", host_from(headers));
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}{path}?{raw}"),
        _ => format!("{base}{path}"),
    };
    let body = history_bundle(&base, &self_url, &page, request.summary);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, FHIR_JSON), (header::CACHE_CONTROL, "no-store")],
        body,
    )
        .into_response())
}

pub async fn search_type(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = type_name.parse::<ResourceType>()?;
    allowed(&state, &headers, DataAction::Read, Some(resource_type)).await?;
    let path = format!("/{resource_type}");
    respond_search(&state, Some(resource_type), path, query, &headers).await
}

pub async fn search_system(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None).await?;
    respond_search(&state, None, String::new(), query, &headers).await
}

pub async fn compartment_search(
    State(state): State<AppState>,
    Path((kind, id, target)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let root_type = kind.parse::<ResourceType>()?;
    allowed(&state, &headers, DataAction::Read, base_of(&target)?).await?;
    let def = fhir_core::search::compartment::definition(root_type.as_str())
        .ok_or_else(|| Error::UnsupportedParameter(format!("compartment {kind:?}")))?;
    let root = ResourceId::parse(&id)?;
    let (base_type, types) = match target.as_str() {
        "*" => (
            None,
            def.types()
                .iter()
                .map(|name| name.parse::<ResourceType>())
                .collect::<Result<Vec<ResourceType>, Error>>()?,
        ),
        name => {
            let one = name.parse::<ResourceType>()?;
            if def.member(one).is_none() {
                return Err(Error::UnsupportedParameter(format!(
                    "{name:?} is not gathered by compartment {kind:?}"
                ))
                .into());
            }
            (Some(one), vec![one])
        }
    };
    let mut request = SearchRequest::parse(&state.registry, base_type, query.as_deref())?;
    request.query.types = types;
    request.query.compartment = Some(Compartment {
        kind: root_type,
        id: root,
    });
    let path = format!("/{}/{}/{}", root_type.as_str(), id, target);
    respond_page(&state, request, path, query, &headers).await
}

pub async fn parameter_status(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None).await?;
    let wanted = param(query.as_deref(), "url");
    Ok(rendered(parameter::status_report(&state, wanted.as_deref())?))
}

pub async fn parameter_status_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None).await?;
    let wanted = match body.is_empty() {
        true => None,
        false => {
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| Error::InvalidJson(error.to_string()))?;
            parameter_value(&value, "url")
        }
    };
    Ok(rendered(parameter::status_report(&state, wanted.as_deref())?))
}

pub async fn parameter_status_update(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::ParameterManagement, None).await?;
    let url = param(query.as_deref(), "url")
        .ok_or_else(|| Error::InvalidParameter("status needs a url".to_owned()))?;
    let wanted = param(query.as_deref(), "status")
        .ok_or_else(|| Error::InvalidParameter("status needs a status".to_owned()))?
        .parse::<fhir_core::search::ParamStatus>()?;
    parameter::set_status(&state, &url, wanted).await?;
    Ok(rendered(parameter::status_report(&state, Some(&url))?))
}

pub async fn parameter_reindex(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::ParameterManagement, None).await?;
    let wanted = param(query.as_deref(), "url");
    Ok(rendered(parameter::reindex(&state, wanted.as_deref()).await?))
}

pub async fn parameter_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::ParameterManagement, None).await?;
    parameter::refresh(&state).await?;
    Ok(rendered(parameter::status_report(&state, None)?))
}

fn parameter_value(body: &Value, name: &str) -> Option<String> {
    body.get("parameter")?
        .as_array()?
        .iter()
        .find(|entry| entry["name"] == name)
        .and_then(|entry| {
            entry["valueUri"]
                .as_str()
                .or_else(|| entry["valueString"].as_str())
                .or_else(|| entry["valueCode"].as_str())
        })
        .map(str::to_owned)
}

pub async fn compartment_definitions(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let base = format!("http://{}", host_from(&headers));
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}/CompartmentDefinition?{raw}"),
        _ => format!("{base}/CompartmentDefinition"),
    };
    Ok(rendered(definitions_bundle(state.version, &base, &self_url)))
}

pub async fn compartment_definition(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let base = format!("http://{}", host_from(&headers));
    let def = fhir_core::search::compartment::definition_in(state.version, &code)
        .ok_or(Error::NotFound)?;
    let body = serde_json::to_vec(&definition_json(&def, &base))
        .map_err(|error| Error::Internal(error.to_string()))?;
    Ok(rendered(body))
}

fn rendered(body: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, FHIR_JSON), (header::CACHE_CONTROL, "no-store")],
        body,
    )
        .into_response()
}

pub(crate) fn grant_of(headers: &HeaderMap) -> Result<Option<Grant>, Error> {
    match headers.get(SCOPE) {
        None => Ok(None),
        Some(value) => {
            let raw = value
                .to_str()
                .map_err(|_| Error::InvalidParameter("scope is not ascii".to_owned()))?;
            Ok(Some(Grant::parse(raw)?))
        }
    }
}

pub(crate) fn confine(query: &mut SearchQuery, grant: Option<Grant>) -> Result<(), Error> {
    let Some(grant) = grant else { return Ok(()) };
    if let Some(refused) = query.types.iter().find(|kind| !grant.admits(**kind)) {
        return Err(Error::Forbidden(format!("type {:?}", refused.as_str())));
    }
    query.grant = Some(grant);
    Ok(())
}

async fn respond_search(
    state: &AppState,
    base_type: Option<ResourceType>,
    path: String,
    query: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let request = SearchRequest::parse(&state.registry, base_type, query.as_deref())?;
    respond_page(state, request, path, query, headers).await
}

pub(crate) async fn respond_page(
    state: &AppState,
    mut request: SearchRequest,
    path: String,
    query: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let access = crate::access::access_of(state, headers).await?;
    confine(
        &mut request.query,
        confining(state, &access, headers, DataAction::Read)?,
    )?;
    crate::terminology::resolve(state.terminology.as_ref(), &mut request.query).await?;
    let page = state.store.search(&request.query).await?;
    let base = format!("http://{}", host_from(headers));
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}{path}?{raw}"),
        _ => format!("{base}{path}"),
    };
    let body = search_bundle(&base, &self_url, &page, request.summary, &request.elements);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, FHIR_JSON), (header::CACHE_CONTROL, "no-store")],
        body,
    )
        .into_response())
}

pub(crate) async fn allowed(
    state: &AppState,
    headers: &HeaderMap,
    action: DataAction,
    resource_type: Option<ResourceType>,
) -> Result<Access, Error> {
    let access = crate::access::access_of(state, headers).await?;
    access.require(action, resource_type)?;
    Ok(access)
}

pub(crate) fn base_of(target: &str) -> Result<Option<ResourceType>, Error> {
    match target {
        "*" => Ok(None),
        name => Ok(Some(name.parse::<ResourceType>()?)),
    }
}

pub(crate) fn confining(
    state: &AppState,
    access: &Access,
    headers: &HeaderMap,
    action: DataAction,
) -> Result<Option<Grant>, Error> {
    match access.secured {
        true => crate::access::granted(&state.registry, access, action),
        false => grant_of(headers),
    }
}

pub(crate) fn within(
    state: &AppState,
    access: &Access,
    headers: &HeaderMap,
    action: DataAction,
    envelope: &ResourceEnvelope,
) -> Result<(), Error> {
    let Some(grant) = confining(state, access, headers, action)? else {
        return Ok(());
    };
    let body = serde_json::from_slice::<Value>(envelope.raw()).unwrap_or(Value::Null);
    match grant.reaches(envelope, &body) {
        true => Ok(()),
        false => Err(Error::NotFound),
    }
}

fn covers(grant: &Option<Grant>, scope: &HistoryScope) -> Result<(), Error> {
    let Some(grant) = grant else { return Ok(()) };
    let refused = |what: &str| Err(Error::Forbidden(format!("{what} history under this grant")));
    match scope {
        HistoryScope::Instance(_, _) => Ok(()),
        HistoryScope::Type(kind) => match grant.admits(*kind)
            && grant.is_open()
            && grant.narrowing(*kind).is_empty()
        {
            true => Ok(()),
            false => refused("type"),
        },
        HistoryScope::System => {
            match grant.types.is_empty() && grant.is_open() && grant.filters.is_empty() {
                true => Ok(()),
                false => refused("system"),
            }
        }
    }
}
