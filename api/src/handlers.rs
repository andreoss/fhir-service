use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::search::{Compartment, Grant, ParameterSpec};
use fhir_core::security::scope::DataAction;
use fhir_core::security::Access;
use fhir_core::{
    Error, FhirInstant, IssueCode, IssueSeverity, OperationOutcome, Patch, ResourceEnvelope,
    ResourceId, ResourceKey, ResourceType, VersionId, WeakEtag,
};
use fhir_store::{AuditEvent, HistoryScope, Interaction, SearchQuery};
use serde_json::Value;
use uuid::Uuid;

use crate::app::AppState;
use crate::capabilities::ConditionalDelete;
use crate::compartment::{definition_json, definitions_bundle};
use crate::conditional;
use crate::history::{history_bundle, HistoryRequest};
use crate::parameter::{self, SEARCH_PARAMETER};
use crate::preference::Return;
use crate::query::param;
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
        let status = StatusCode::from_u16(outcome.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (
            status,
            [
                (header::CONTENT_TYPE, FHIR_JSON),
                (header::CACHE_CONTROL, "no-store"),
            ],
            outcome.to_fhir_json(),
        )
            .into_response();
        if let Some(seconds) = self.0.retry_after() {
            if let Ok(value) = header::HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

pub async fn read(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let access = allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let envelope = state.store.read(&key(resource_type, &id)).await?;
    if envelope.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    if envelope.is_deleted() {
        return Err(Error::Deleted.into());
    }
    within(&state, &access, &headers, DataAction::Read, &envelope)?;
    let host = &addressed(&state, &headers);
    let precondition = conditional::asked_for(&headers);
    if conditional::holds(precondition.as_ref(), &envelope) {
        return Ok(respond_not_modified(&envelope, host));
    }
    Ok(respond_resource(&envelope, host))
}

pub async fn vread(
    State(state): State<AppState>,
    Path((type_name, id_text, version_text)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let access = allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    state.tenancy.refuses("reading a past version")?;
    let version = version_text.parse::<VersionId>()?;
    let envelope = state
        .store
        .vread(&key(resource_type, &id), &version)
        .await?;
    if envelope.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    if envelope.is_deleted() {
        return Err(Error::Deleted.into());
    }
    within(&state, &access, &headers, DataAction::Read, &envelope)?;
    Ok(respond_resource(&envelope, &addressed(&state, &headers)))
}

pub async fn create(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let access = crate::access::access_of(&state, &headers).await?;
    let asked = Return::asked_for(&headers);
    let provenance = crate::provenance::carried(&headers, state.version)?;
    let mut value: Value =
        serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    tenanted(&state, &access, &headers, &mut value)?;
    shortened(&state, &headers, &mut value);
    let id = body_id(&value)?;
    judged(
        &state,
        &access,
        DataAction::Write,
        Interaction::Create,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    if let Some(condition) = headers.get(IF_NONE_EXIST) {
        let raw = condition
            .to_str()
            .map_err(|_| Error::InvalidEnvelope("if-none-exist is not ascii".to_owned()))?;
        let query = require_condition(
            parse_query(&state.registry, Some(resource_type), Some(raw))?,
            "if-none-exist",
        )?;
        if let Some(existing) = single_match(&state, &query).await? {
            return Ok(answered(
                respond_updated(&existing, &addressed(&state, &headers)),
                &existing,
                asked,
            ));
        }
    }
    let envelope = write_envelope(state.version, resource_type, value.clone(), &id)?;
    within(&state, &access, &headers, DataAction::Write, &envelope)?;
    profiled(&state, true, &value).await?;
    if resource_type.as_str() == SEARCH_PARAMETER {
        let spec = ParameterSpec::parse(&value)?;
        let _guard = state.parameters.lock().await;
        parameter::accepts(&state, &spec)?;
        let stored = state.store.create(envelope).await?;
        parameter::install(&state, &spec).await?;
        provenanced(&state, &access, provenance, &stored).await?;
        return Ok(answered(
            respond_created(&stored, &addressed(&state, &headers)),
            &stored,
            asked,
        ));
    }
    let stored = state.store.create(envelope).await?;
    registered_type(&value)?;
    provenanced(&state, &access, provenance, &stored).await?;
    Ok(answered(
        respond_created(&stored, &addressed(&state, &headers)),
        &stored,
        asked,
    ))
}



fn registered_type(value: &Value) -> Result<(), Error> {
    match crate::profile::defines_a_type(value) {
        None => Ok(()),
        Some(name) => fhir_core::resource_type::register(&name).map(|_| ()),
    }
}

pub async fn conditional_update(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let access = allowed(
        &state,
        &headers,
        DataAction::Write,
        Some(resource_type),
        None,
    )
    .await?;
    let mut selection = require_condition(
        parse_query(&state.registry, Some(resource_type), query.as_deref())?,
        "conditional update",
    )?;
    confine(
        &mut selection,
        confining(&state, &access, &headers, DataAction::Write)?,
    )?;
    let asked = Return::asked_for(&headers);
    let provenance = crate::provenance::carried(&headers, state.version)?;
    let mut value: Value =
        serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    tenanted(&state, &access, &headers, &mut value)?;
    shortened(&state, &headers, &mut value);
    let expected = expected_version(&headers)?;
    match single_match(&state, &selection).await? {
        Some(existing) => {
            let envelope =
                write_envelope(state.version, resource_type, value.clone(), existing.id())?;
            profiled(&state, false, &value).await?;
            under_policy(&state, resource_type, expected.as_ref(), true)?;
            let written = upsert(&state, envelope, expected.as_ref()).await?;
            after_policy(&state, written.envelope()).await?;
            provenanced(&state, &access, provenance, written.envelope()).await?;
            Ok(written.respond(&addressed(&state, &headers), asked))
        }
        None if !state.capabilities.create_on_update => Err(Error::NotFound.into()),
        None => {
            let id = body_id(&value)?;
            let envelope = write_envelope(state.version, resource_type, value.clone(), &id)?;
            profiled(&state, true, &value).await?;
            let stored = state.store.create(envelope).await?;
            provenanced(&state, &access, provenance, &stored).await?;
            Ok(answered(
                respond_created(&stored, &addressed(&state, &headers)),
                &stored,
                asked,
            ))
        }
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let access = allowed(
        &state,
        &headers,
        DataAction::Write,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let expected = expected_version(&headers)?;
    let provenance = crate::provenance::carried(&headers, state.version)?;
    let mut value: Value =
        serde_json::from_slice(&body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    tenanted(&state, &access, &headers, &mut value)?;
    shortened(&state, &headers, &mut value);
    let envelope = write_envelope(state.version, resource_type, value.clone(), &id)?;
    within(&state, &access, &headers, DataAction::Write, &envelope)?;
    over_current(&state, &access, &headers, resource_type, &id).await?;
    profiled(
        &state,
        matches!(presence(&state, resource_type, &id).await, Presence::Absent),
        &value,
    )
    .await?;
    if resource_type.as_str() == SEARCH_PARAMETER {
        return replace_parameter(&state, &id, &value, envelope, expected, &headers).await;
    }
    under_policy(
        &state,
        resource_type,
        expected.as_ref(),
        !matches!(presence(&state, resource_type, &id).await, Presence::Absent),
    )?;
    if !state.capabilities.create_on_update
        && matches!(presence(&state, resource_type, &id).await, Presence::Absent)
    {
        return Err(Error::NotFound.into());
    }
    let written = upsert(&state, envelope, expected.as_ref()).await?;
    registered_type(&value)?;
    after_policy(&state, written.envelope()).await?;
    provenanced(&state, &access, provenance, written.envelope()).await?;
    Ok(written.respond(&addressed(&state, &headers), Return::asked_for(&headers)))
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
    let previous = state
        .store
        .read(&key(SEARCH_PARAMETER.parse()?, id))
        .await
        .ok();
    let written = upsert(state, envelope, expected.as_ref()).await?;
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
    Ok(written.respond(&addressed(state, headers), Return::asked_for(headers)))
}

pub async fn delete_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let access = allowed_doing(
        &state,
        &headers,
        DataAction::Write,
        Interaction::Delete,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let current = match state.store.read(&key(resource_type, &id)).await {
        Ok(current) if current.resource_type() == resource_type => current,
        Ok(_) | Err(Error::NotFound) => return Ok(no_content(None)),
        Err(error) => return Err(error.into()),
    };
    within(&state, &access, &headers, DataAction::Write, &current)?;
    let removed = remove(&state, resource_type, &id, hard_delete(query.as_deref())).await?;
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
    let resource_type = served_here(&state, &type_name)?;
    let access = allowed_doing(
        &state,
        &headers,
        DataAction::Write,
        Interaction::Delete,
        Some(resource_type),
        None,
    )
    .await?;
    let mut selection = require_condition(
        parse_query(&state.registry, Some(resource_type), query.as_deref())?,
        "conditional delete",
    )?;
    confine(
        &mut selection,
        confining(&state, &access, &headers, DataAction::Write)?,
    )?;
    let hard = hard_delete(query.as_deref());
    let most = state.capabilities.conditional_delete.most();
    selection.count = most.saturating_add(1);
    let found = state.store.search(&selection).await?.entries;
    match (found.len(), state.capabilities.conditional_delete) {
        (0, _) => Ok(no_content(None)),
        (1, _) => remove(&state, resource_type, found[0].id(), hard).await,
        (_, ConditionalDelete::Single) => Err(Error::MultipleMatches.into()),
        (held, ConditionalDelete::Multiple(most)) if held > most => {
            Err(Error::MultipleMatches.into())
        }
        (_, ConditionalDelete::Multiple(_)) => {
            for entry in &found {
                remove(&state, resource_type, entry.id(), hard).await?;
            }
            Ok(no_content(None))
        }
    }
}

pub async fn patch_instance(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let access = allowed(
        &state,
        &headers,
        DataAction::Write,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let current = state.store.read(&key(resource_type, &id)).await?;
    if current.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    within(&state, &access, &headers, DataAction::Write, &current)?;
    if current.is_deleted() {
        return Err(Error::Deleted.into());
    }
    patch_stored(&state, &access, resource_type, &current, &headers, &body).await
}

pub async fn conditional_patch(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let access = allowed(
        &state,
        &headers,
        DataAction::Write,
        Some(resource_type),
        None,
    )
    .await?;
    let mut selection = require_condition(
        parse_query(&state.registry, Some(resource_type), query.as_deref())?,
        "conditional patch",
    )?;
    confine(
        &mut selection,
        confining(&state, &access, &headers, DataAction::Write)?,
    )?;
    match single_match(&state, &selection).await? {
        Some(existing) => {
            patch_stored(&state, &access, resource_type, &existing, &headers, &body).await
        }
        None => Err(Error::NotFound.into()),
    }
}

pub async fn purge_history(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let access = allowed_doing(
        &state,
        &headers,
        DataAction::Write,
        Interaction::Delete,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let current = state.store.read(&key(resource_type, &id)).await?;
    if current.resource_type() != resource_type {
        return Err(Error::NotFound.into());
    }
    within(&state, &access, &headers, DataAction::Write, &current)?;
    let purged = state.store.purge_history(&key(resource_type, &id)).await?;
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
    let mut dependencies: Vec<Value> = Vec::new();
    for dependency in state.dependencies.iter() {
        match (dependency.check)().await {
            Ok(()) => {
                dependencies.push(serde_json::json!({ "name": dependency.name, "status": "ok" }))
            }
            Err(message) => {
                any_failure = true;
                dependencies.push(
                    serde_json::json!({ "name": dependency.name, "status": "error", "detail": message }),
                );
            }
        }
    }
    let status = if any_failure { "degraded" } else { "ok" };
    let status_code = if any_failure {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    let body =
        serde_json::to_vec(&serde_json::json!({ "status": status, "dependencies": dependencies }))
            .expect("health payload is serializable");
    (
        status_code,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
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





fn under_policy(
    state: &AppState,
    resource_type: ResourceType,
    expected: Option<&VersionId>,
    existed: bool,
) -> Result<(), Error> {
    let policy = state.versioning.of(resource_type);
    if policy.needs_match() && expected.is_none() && existed {
        return Err(Error::VersionRequired(format!(
            "{resource_type} is held under {}, so an update names the version it replaces with If-Match",
            policy.as_str()
        )));
    }
    Ok(())
}




async fn after_policy(state: &AppState, written: &ResourceEnvelope) -> Result<(), Error> {
    if state.versioning.of(written.resource_type()).keeps_history() {
        return Ok(());
    }
    state.store.purge_history(&ResourceKey::of(written)).await?;
    Ok(())
}



async fn profiled(state: &AppState, created: bool, value: &Value) -> Result<(), Error> {
    if !state.allowed_profiles.accepts(value) {
        return Err(Error::NoMatch(format!(
            "this instance accepts a resource claiming one of {}, and this one claims none",
            state.allowed_profiles.named().join(", ")
        )));
    }
    let asked = match created {
        true => state.profiles.create,
        false => state.profiles.update,
    };
    if !asked {
        return Ok(());
    }
    crate::profile::judged_for_write(&state.store, state.terminology.as_ref(), value).await
}

async fn provenanced(
    state: &AppState,
    access: &Access,
    carried: Option<Value>,
    written: &ResourceEnvelope,
) -> Result<(), Error> {
    crate::provenance::record(
        state,
        access,
        carried,
        &[crate::provenance::reference_of(written)],
    )
    .await
}

impl Written {
    fn envelope(&self) -> &ResourceEnvelope {
        match self {
            Written::Created(stored) | Written::Updated(stored) | Written::Unchanged(stored) => {
                stored
            }
        }
    }
}

enum Written {
    Created(ResourceEnvelope),
    Updated(ResourceEnvelope),
    
    
    
    Unchanged(ResourceEnvelope),
}

impl Written {
    fn respond(&self, host: &str, asked: Option<Return>) -> Response {
        match self {
            Written::Created(stored) => answered(respond_created(stored, host), stored, asked),
            Written::Updated(stored) => answered(respond_updated(stored, host), stored, asked),
            Written::Unchanged(stored) => match asked {
                
                
                Some(Return::Outcome) => {
                    let (mut parts, _) = respond_updated(stored, host).into_parts();
                    let body = crate::unchanged::outcome().to_fhir_json();
                    parts
                        .headers
                        .insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
                    parts.headers.insert(
                        header::CONTENT_LENGTH,
                        HeaderValue::from_str(&body.len().to_string())
                            .expect("a length is a header value"),
                    );
                    Response::from_parts(parts, Body::from(body))
                }
                asked => answered(respond_updated(stored, host), stored, asked),
            },
        }
    }
}

fn answered(response: Response, envelope: &ResourceEnvelope, asked: Option<Return>) -> Response {
    match asked {
        None | Some(Return::Representation) => response,
        Some(Return::Minimal) => emptied(response),
        Some(Return::Outcome) => {
            let (mut parts, _) = response.into_parts();
            let outcome = OperationOutcome {
                id: None,
                severity: IssueSeverity::Information,
                code: IssueCode::Informational,
                diagnostics: Some(format!(
                    "the {} {} was carried out at version {}",
                    envelope.resource_type(),
                    envelope.id(),
                    envelope.version_id()
                )),
            };
            parts
                .headers
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
            let body = outcome.to_fhir_json();
            parts.headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&body.len().to_string()).expect("a length is a header value"),
            );
            Response::from_parts(parts, Body::from(body))
        }
    }
}

fn emptied(response: Response) -> Response {
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_TYPE);
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    Response::from_parts(parts, Body::empty())
}

fn contended(version: fhir_core::FhirVersion) -> Error {
    match version {
        fhir_core::FhirVersion::Stu3 => Error::VersionConflict,
        _ => Error::StaleVersion,
    }
}

enum Presence {
    Live,
    Deleted,
    Absent,
}

async fn presence(state: &AppState, resource_type: ResourceType, id: &ResourceId) -> Presence {
    match state.store.read(&key(resource_type, id)).await {
        Ok(current) if current.is_deleted() => Presence::Deleted,
        Ok(_) => Presence::Live,
        Err(_) => Presence::Absent,
    }
}

async fn upsert(
    state: &AppState,
    envelope: ResourceEnvelope,
    expected: Option<&VersionId>,
) -> Result<Written, Error> {
    let offered = envelope.clone();
    
    
    
    
    
    
    
    let before = match state.unchanged.is_on() {
        false => None,
        true => state.store.read(&ResourceKey::of(&envelope)).await.ok(),
    };
    if let Some(current) = &before {
        if !current.is_deleted() {
            let stored = serde_json::from_slice::<Value>(current.raw()).unwrap_or(Value::Null);
            let sent = serde_json::from_slice::<Value>(envelope.raw()).unwrap_or(Value::Null);
            if state.unchanged.holds(&sent, &stored) {
                return Ok(Written::Unchanged(current.clone()));
            }
        }
    }
    let recreated = matches!(
        presence(state, envelope.resource_type(), envelope.id()).await,
        Presence::Deleted
    );
    let held = before.map(|current| current.version_id().clone());
    match state.store.update(envelope, expected).await {
        Ok(stored) if recreated => Ok(Written::Created(stored)),
        
        
        Ok(stored) if held.as_ref() == Some(stored.version_id()) => Ok(Written::Unchanged(stored)),
        Ok(stored) => Ok(Written::Updated(stored)),
        Err(Error::VersionConflict) => Err(contended(state.version)),
        Err(Error::NotFound) if expected.is_none() => {
            state.store.create(offered).await.map(Written::Created)
        }
        Err(error) => Err(error),
    }
}

async fn patch_stored(
    state: &AppState,
    access: &Access,
    resource_type: ResourceType,
    current: &ResourceEnvelope,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response, AppError> {
    let provenance = crate::provenance::carried(headers, state.version)?;
    let patched = Patch::parse(body)?.apply(current.raw())?;
    let mut value: Value =
        serde_json::from_slice(&patched).map_err(|error| Error::InvalidJson(error.to_string()))?;
    
    
    tenanted(state, access, headers, &mut value)?;
    shortened(state, headers, &mut value);
    let envelope = write_envelope(state.version, resource_type, value.clone(), current.id())?;
    within(state, access, headers, DataAction::Write, current)?;
    profiled(state, false, &value).await?;
    let expected = expected_version(headers)?;
    under_policy(state, resource_type, expected.as_ref(), true)?;
    let written = upsert(state, envelope, expected.as_ref()).await?;
    after_policy(state, written.envelope()).await?;
    provenanced(state, access, provenance, written.envelope()).await?;
    Ok(written.respond(&addressed(state, headers), Return::asked_for(headers)))
}

async fn remove(
    state: &AppState,
    resource_type: ResourceType,
    id: &ResourceId,
    hard: bool,
) -> Result<Response, AppError> {
    let held = key(resource_type, id);
    if hard {
        state.store.hard_delete(&held).await?;
        return Ok(no_content(None));
    }
    match state.store.delete(&held).await {
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
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&etag(marker)).expect("etag is a header value"),
        );
    }
    response
}

fn require_condition(query: SearchQuery, what: &str) -> Result<SearchQuery, Error> {
    if query.is_unconditional() {
        return Err(Error::InvalidEnvelope(format!(
            "{what} requires search parameters"
        )));
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


fn key(resource_type: ResourceType, id: &ResourceId) -> ResourceKey {
    ResourceKey::new(resource_type, id.clone())
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
            let text = value
                .to_str()
                .map_err(|_| Error::InvalidEtag("if-match is not ascii".to_owned()))?;
            Ok(Some(
                WeakEtag::try_from(text)?.as_str().parse::<VersionId>()?,
            ))
        }
        None => Ok(None),
    }
}

fn respond_resource(envelope: &ResourceEnvelope, host: &str) -> Response {
    let mut response = Response::new(Body::from(envelope.raw().to_vec()));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&etag(envelope)).expect("etag is a header value"),
    );
    headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&last_modified(envelope.last_updated()))
            .expect("last-modified is a header value"),
    );
    headers.insert(
        header::CONTENT_LOCATION,
        HeaderValue::from_str(&location(host, envelope))
            .expect("content-location is a header value"),
    );
    response
}

fn respond_not_modified(envelope: &ResourceEnvelope, host: &str) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    let headers = response.headers_mut();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&etag(envelope)).expect("etag is a header value"),
    );
    headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&last_modified(envelope.last_updated()))
            .expect("last-modified is a header value"),
    );
    headers.insert(
        header::CONTENT_LOCATION,
        HeaderValue::from_str(&location(host, envelope))
            .expect("content-location is a header value"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn respond_created(envelope: &ResourceEnvelope, host: &str) -> Response {
    let mut response = respond_resource(envelope, host);
    *response.status_mut() = StatusCode::CREATED;
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location(host, envelope)).expect("location is a header value"),
    );
    response
}

fn respond_updated(envelope: &ResourceEnvelope, host: &str) -> Response {
    let mut response = respond_resource(envelope, host);
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location(host, envelope)).expect("location is a header value"),
    );
    response
}

fn etag(envelope: &ResourceEnvelope) -> String {
    WeakEtag::from(envelope.version_id()).to_string()
}




fn location(base: &str, envelope: &ResourceEnvelope) -> String {
    let base = match base.trim().is_empty() {
        true => "http://localhost",
        false => base.trim(),
    };
    format!(
        "{base}/{}/{}/_history/{}",
        envelope.resource_type(),
        envelope.id(),
        envelope.version_id()
    )
}


pub(crate) fn addressed(state: &AppState, headers: &HeaderMap) -> String {
    state.forwarding.base(headers)
}

fn last_modified(instant: &FhirInstant) -> String {
    match time::OffsetDateTime::parse(
        instant.as_str(),
        &time::format_description::well_known::Rfc3339,
    ) {
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
        return Err(Error::InvalidEnvelope(
            "resource type does not match the request path".to_owned(),
        ));
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
        Some(false) => {
            return Err(Error::InvalidEnvelope(
                "id does not match the request path".to_owned(),
            ))
        }
        Some(true) => {}
    }
    fhir_core::with_assigned_meta(&mut value)?;
    matches_definitions(version, &value)?;
    let bytes =
        serde_json::to_vec(&value).map_err(|error| Error::InvalidJson(error.to_string()))?;
    ResourceEnvelope::parse(version, &bytes)
}

fn matches_definitions(version: fhir_core::FhirVersion, value: &Value) -> Result<(), Error> {
    let findings = fhir_core::Model::of(version).check(value);
    match findings.is_empty() {
        true => Ok(()),
        false => Err(Error::InvalidEnvelope(refusal(&findings))),
    }
}

fn refusal(findings: &[fhir_core::Finding]) -> String {
    let listed: Vec<String> = findings
        .iter()
        .take(3)
        .map(|finding| format!("{} at {}: {}", finding.rule, finding.path, finding.detail))
        .collect();
    match findings.len() > listed.len() {
        true => format!(
            "{}; and {} more",
            listed.join("; "),
            findings.len() - listed.len()
        ),
        false => listed.join("; "),
    }
}
pub async fn system_history(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    respond_history(
        &state,
        HistoryScope::System,
        "/_history".to_owned(),
        query,
        &headers,
    )
    .await
}

pub async fn type_history(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        None,
    )
    .await?;
    let path = format!("/{resource_type}/_history");
    respond_history(
        &state,
        HistoryScope::Type(resource_type),
        path,
        query,
        &headers,
    )
    .await
}

pub async fn instance_history(
    State(state): State<AppState>,
    Path((type_name, id_text)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    let id = id_text.parse::<ResourceId>()?;
    let access = allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        Some(&id),
    )
    .await?;
    let path = format!("/{resource_type}/{id}/_history");
    let current = state.store.read(&key(resource_type, &id)).await?;
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
    state.tenancy.refuses("history")?;
    let request = HistoryRequest::parse(query.as_deref())?;
    let access = crate::access::access_of(state, headers).await?;
    covers(
        &confining(state, &access, headers, DataAction::Read)?,
        &scope,
    )?;
    let page = state.store.history(&scope, &request.query).await?;
    let base = addressed(state, headers);
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}{path}?{raw}"),
        _ => format!("{base}{path}"),
    };
    let body = history_bundle(&base, &self_url, &page, request.summary, state.version);
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
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
    let resource_type = served_here(&state, &type_name)?;
    allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        None,
    )
    .await?;
    let path = format!("/{resource_type}");
    let asked = crate::interaction::Asked::new(
        crate::interaction::SEARCH,
        path.clone(),
        Some(resource_type.as_str()),
        query.as_deref(),
        addressed(&state, &headers),
    );
    if let Some(answered) = crate::job::deferred(&state, &asked, &headers).await {
        return Ok(answered);
    }
    respond_search(&state, Some(resource_type), path, query, &headers).await
}




pub async fn search_type_form(
    State(state): State<AppState>,
    Path(type_name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let resource_type = served_here(&state, &type_name)?;
    allowed(
        &state,
        &headers,
        DataAction::Read,
        Some(resource_type),
        None,
    )
    .await?;
    let asked = merged_query(query, &body)?;
    let path = format!("/{resource_type}/_search");
    respond_search(&state, Some(resource_type), path, asked, &headers).await
}

pub async fn search_system_form(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = merged_query(query, &body)?;
    respond_search(&state, None, "/_search".to_owned(), asked, &headers).await
}

fn merged_query(query: Option<String>, body: &[u8]) -> Result<Option<String>, Error> {
    let form = std::str::from_utf8(body)
        .map_err(|_| Error::InvalidEnvelope("the form is not utf-8".to_owned()))?
        .trim();
    let held: Vec<&str> = [query.as_deref().unwrap_or_default(), form]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    Ok(match held.is_empty() {
        true => None,
        false => Some(held.join("&")),
    })
}

pub async fn search_system(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let asked = crate::interaction::Asked::new(
        crate::interaction::SEARCH,
        "/",
        None,
        query.as_deref(),
        addressed(&state, &headers),
    );
    if let Some(answered) = crate::job::deferred(&state, &asked, &headers).await {
        return Ok(answered);
    }
    respond_search(&state, None, String::new(), query, &headers).await
}

pub async fn compartment_search(
    State(state): State<AppState>,
    Path((kind, id, target)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let root_type = kind.parse::<ResourceType>()?;
    allowed(&state, &headers, DataAction::Read, base_of(&target)?, None).await?;
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
    let (mut request, kept) = parsed_search(&state, base_type, query.as_deref(), &headers)?;
    request.query.types = types;
    request.query.compartment = Some(Compartment {
        kind: root_type,
        id: root,
    });
    let path = format!("/{}/{}/{}", root_type.as_str(), id, target);
    respond_page(&state, request, path, kept, &headers).await
}

pub async fn parameter_status(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let wanted = param(query.as_deref(), "url");
    Ok(rendered(
        parameter::status_report(&state, wanted.as_deref()).await?,
    ))
}

pub async fn parameter_status_of(
    State(state): State<AppState>,
    Path(id_text): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let id = id_text.parse::<ResourceId>()?;
    let stored = state
        .store
        .read(&key(SEARCH_PARAMETER.parse()?, &id))
        .await?;
    if stored.resource_type().as_str() != SEARCH_PARAMETER || stored.is_deleted() {
        return Err(Error::NotFound.into());
    }
    let body: Value = serde_json::from_slice(stored.raw())
        .map_err(|error| Error::InvalidJson(error.to_string()))?;
    let url = body.get("url").and_then(Value::as_str).ok_or_else(|| {
        Error::InvalidEnvelope(format!("{SEARCH_PARAMETER} {id_text} has no url"))
    })?;
    Ok(rendered(parameter::status_report(&state, Some(url)).await?))
}

pub async fn parameter_status_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let raw = std::str::from_utf8(&body)
        .map_err(|_| Error::InvalidEnvelope("the form is not utf-8".to_owned()))?;
    let wanted = param(Some(raw), "url");
    Ok(rendered(
        parameter::status_report(&state, wanted.as_deref()).await?,
    ))
}

pub async fn parameter_status_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    allowed(&state, &headers, DataAction::Read, None, None).await?;
    let wanted = match body.is_empty() {
        true => None,
        false => {
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| Error::InvalidJson(error.to_string()))?;
            parameter_value(&value, "url")
        }
    };
    Ok(rendered(
        parameter::status_report(&state, wanted.as_deref()).await?,
    ))
}

pub async fn parameter_status_update(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(
        &state,
        &headers,
        DataAction::ParameterManagement,
        None,
        None,
    )
    .await?;
    let url = param(query.as_deref(), "url")
        .ok_or_else(|| Error::InvalidParameter("status needs a url".to_owned()))?;
    let wanted = param(query.as_deref(), "status")
        .ok_or_else(|| Error::InvalidParameter("status needs a status".to_owned()))?
        .parse::<fhir_core::search::ParamStatus>()?;
    parameter::set_status(&state, &url, wanted).await?;
    Ok(rendered(
        parameter::status_report(&state, Some(&url)).await?,
    ))
}

pub async fn parameter_reindex(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(
        &state,
        &headers,
        DataAction::ParameterManagement,
        None,
        None,
    )
    .await?;
    let wanted = param(query.as_deref(), "url");
    
    
    let _held = state.busy.during("a reindex");
    Ok(rendered(
        parameter::reindex(&state, wanted.as_deref()).await?,
    ))
}

pub async fn parameter_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    allowed(
        &state,
        &headers,
        DataAction::ParameterManagement,
        None,
        None,
    )
    .await?;
    parameter::refresh(&state).await?;
    Ok(rendered(parameter::status_report(&state, None).await?))
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
    let base = addressed(&state, &headers);
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}/CompartmentDefinition?{raw}"),
        _ => format!("{base}/CompartmentDefinition"),
    };
    Ok(rendered(definitions_bundle(
        state.version,
        &base,
        &self_url,
    )))
}

pub async fn compartment_definition(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let base = addressed(&state, &headers);
    let def = fhir_core::search::compartment::definition_in(state.version, &code)
        .ok_or(Error::NotFound)?;
    let body = serde_json::to_vec(&definition_json(&def, &base))
        .map_err(|error| Error::Internal(error.to_string()))?;
    Ok(rendered(body))
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
    let (request, kept) = parsed_search(state, base_type, query.as_deref(), headers)?;
    respond_page(state, request, path, kept, headers).await
}




fn paged(state: &AppState, request: &mut SearchRequest) -> Result<(), Error> {
    let paging = &state.paging;
    request.query.count = match request.named.count {
        true => paging.count_of(Some(request.query.count)),
        false => paging.count_of(None),
    };
    if !request.named.total {
        request.query.total = match paging.total() {
            crate::paging::Counting::None => fhir_store::TotalMode::None,
            crate::paging::Counting::Accurate => fhir_store::TotalMode::Accurate,
        };
    }
    
    
    if matches!(request.summary, crate::history::Summary::Count) {
        request.query.total = fhir_store::TotalMode::Accurate;
        request.query.count = 0;
    }
    if !request.named.sort {
        if let Some(sort) = paging.sort() {
            let base = request.query.types.first().copied();
            request.query.sort = crate::search::sort_keys(&state.registry, base, sort)?;
        }
    }
    Ok(())
}




pub(crate) fn parsed_search(
    state: &AppState,
    base_type: Option<ResourceType>,
    raw: Option<&str>,
    headers: &HeaderMap,
) -> Result<(SearchRequest, Option<String>), Error> {
    let (request, kept) = match crate::preference::Handling::asked_for(headers)?.is_lenient() {
        true => SearchRequest::parse_leniently(&state.registry, base_type, raw)?,
        false => (
            SearchRequest::parse(&state.registry, base_type, raw)?,
            raw.map(str::to_owned),
        ),
    };
    
    
    
    if state.restricted.is_on() {
        if let Some(refused) = request
            .query
            .filters
            .iter()
            .find(|filter| !state.restricted.answers(&filter.name))
        {
            return Err(Error::UnsupportedParameter(format!(
                "{:?} is not among the search parameters this instance answers",
                refused.name
            )));
        }
    }
    Ok((request, kept))
}

pub(crate) async fn respond_page(
    state: &AppState,
    mut request: SearchRequest,
    path: String,
    query: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let access = crate::access::access_of(state, headers).await?;
    request.query.include_depth = state.capabilities.include_depth;
    paged(state, &mut request)?;
    confine(
        &mut request.query,
        confining(state, &access, headers, DataAction::Read)?,
    )?;
    crate::terminology::resolve(state.terminology.as_ref(), &mut request.query).await?;
    crate::membership::resolve(&state.store, &mut request.query).await?;
    let page = state.store.search(&request.query).await?;
    let base = addressed(state, headers);
    let self_url = match query.as_deref() {
        Some(raw) if !raw.is_empty() => format!("{base}{path}?{raw}"),
        _ => format!("{base}{path}"),
    };
    let body = search_bundle(
        &base,
        &self_url,
        &page,
        request.summary,
        &request.elements,
        &request.dropped,
    );
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, FHIR_JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}

pub(crate) async fn allowed(
    state: &AppState,
    headers: &HeaderMap,
    action: DataAction,
    resource_type: Option<ResourceType>,
    id: Option<&ResourceId>,
) -> Result<Access, Error> {
    allowed_doing(
        state,
        headers,
        action,
        Interaction::of(action),
        resource_type,
        id,
    )
    .await
}

pub(crate) async fn allowed_doing(
    state: &AppState,
    headers: &HeaderMap,
    action: DataAction,
    interaction: Interaction,
    resource_type: Option<ResourceType>,
    id: Option<&ResourceId>,
) -> Result<Access, Error> {
    let access = crate::access::access_of(state, headers).await?;
    judged(state, &access, action, interaction, resource_type, id).await?;
    Ok(access)
}

pub(crate) async fn judged(
    state: &AppState,
    access: &Access,
    action: DataAction,
    interaction: Interaction,
    resource_type: Option<ResourceType>,
    id: Option<&ResourceId>,
) -> Result<(), Error> {
    
    
    
    
    let mut decision = access
        .require(action, resource_type)
        .and_then(|()| state.roles.require(access, action, resource_type));
    if decision.is_ok() {
        decision = crate::policy::require(
            state.policies,
            state.store.as_ref(),
            access,
            action,
            resource_type,
        )
        .await;
    }
    let event = AuditEvent::allowed(&access.actor, action)
        .doing(interaction)
        .by(access.client.clone())
        .of(resource_type, id.cloned());
    let recorded = match decision.is_ok() {
        true => event,
        false => event.refused(),
    };
    state.audit.record(recorded).await?;
    decision
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
    let grant = match access.secured {
        true => crate::access::granted(&state.registry, access, action)?,
        false => grant_of(headers)?,
    };
    
    
    
    state.tenancy.confining(grant, access, headers)
}







async fn over_current(
    state: &AppState,
    access: &Access,
    headers: &HeaderMap,
    resource_type: ResourceType,
    id: &ResourceId,
) -> Result<(), Error> {
    let Ok(current) = state.store.read(&key(resource_type, id)).await else {
        return Ok(());
    };
    if current.is_deleted() {
        return Ok(());
    }
    within(state, access, headers, DataAction::Write, &current)
}



fn shortened(state: &AppState, headers: &HeaderMap, value: &mut Value) {
    if state.references.is_on() {
        let base = addressed(state, headers);
        state.references.stored(value, &base);
    }
}



fn tenanted(
    state: &AppState,
    access: &Access,
    headers: &HeaderMap,
    value: &mut Value,
) -> Result<(), Error> {
    if let Some(tenant) = state.tenancy.of(access, headers)? {
        state.tenancy.labelled(value, &tenant);
    }
    Ok(())
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
    if grant.reaches(envelope, &body) {
        return Ok(());
    }
    
    
    match crate::logged_in::reading_itself(access, action, envelope) {
        true => Ok(()),
        false => Err(Error::NotFound),
    }
}

fn covers(grant: &Option<Grant>, scope: &HistoryScope) -> Result<(), Error> {
    let Some(grant) = grant else { return Ok(()) };
    let refused = |what: &str| Err(Error::Forbidden(format!("{what} history under this grant")));
    match scope {
        HistoryScope::Instance(_, _) => Ok(()),
        HistoryScope::Type(kind) => {
            match grant.admits(*kind) && grant.is_open() && grant.narrowing(*kind).is_empty() {
                true => Ok(()),
                false => refused("type"),
            }
        }
        HistoryScope::System => {
            match grant.types.is_empty() && grant.is_open() && grant.filters.is_empty() {
                true => Ok(()),
                false => refused("system"),
            }
        }
    }
}

pub(crate) fn served(version: fhir_core::FhirVersion, name: &str) -> Result<ResourceType, Error> {
    let held = name.parse::<ResourceType>()?;
    match held.served_by(version) {
        true => Ok(held),
        false => Err(Error::InvalidResourceType(format!(
            "{name} is not a resource type of {version}"
        ))),
    }
}




pub(crate) fn served_here(state: &AppState, name: &str) -> Result<ResourceType, Error> {
    let held = served(state.version, name)?;
    match state.restricted.serves(held) {
        true => Ok(held),
        false => Err(state.restricted.refuse(held)),
    }
}



pub async fn description(State(state): State<AppState>) -> Result<Response, AppError> {
    let held = crate::openapi::document(state.version, &crate::app::served());
    let body = serde_json::to_vec(&held)
        .map_err(|error| Error::Internal(format!("the description does not serialise: {error}")))?;
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(response)
}
