use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::Response;
use fhir_core::security::scope::DataAction;
use fhir_core::security::Access;
use fhir_core::Error;
use fhir_core::{IssueCode, OperationOutcome};
use fhir_store::{InteractionEntry, JobId, JobKind, JobRecord, JobRequest, JobState, JobStore};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use crate::app::AppState;
use crate::handlers::AppError;

const FHIR_JSON: &str = "application/fhir+json";
const JSON: &str = "application/json";

const NDJSON: &str = "application/fhir+ndjson";

const OUTCOME: &str = "OperationOutcome";

const LISTED: [&str; 3] = ["_type", "_typeFilter", "includeAssociatedData"];

pub(crate) const DELETE_PARAMS: [&str; 7] = [
    "_type",
    "_exclude",
    "_maxCount",
    "hardDelete",
    "purgeHistory",
    "_hardDelete",
    "_purgeHistory",
];

const DELETE_LISTED: [&str; 2] = ["_type", "_exclude"];

pub(crate) const UPDATE_PARAMS: [&str; 3] = ["_type", "_exclude", "_maxCount"];

pub(crate) const REINDEX_PARAMS: [&str; 3] = ["_type", "_url", "_resource"];

const REINDEX_LISTED: [&str; 3] = ["_type", "_url", "_resource"];

pub(crate) const ACCEPTED_PARAMS: [&str; 11] = [
    "_type",
    "_typeFilter",
    "_since",
    "_until",
    "_till",
    "_outputFormat",
    "_container",
    "_anonymizationConfig",
    "_anonymizationConfigEtag",
    "_anonymizationConfigCollectionReference",
    "includeAssociatedData",
];

pub const JOBS: &str = "/_jobs";

pub const RETRY_AFTER: u64 = 1;

fn queue(state: &AppState) -> Option<Arc<dyn JobStore>> {
    state.jobs.as_ref().map(Arc::clone)
}

fn sink(state: &AppState) -> Option<Arc<dyn fhir_store::BulkStore>> {
    state.outputs.as_ref().map(Arc::clone)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    Strict,
    Lenient,
}

fn handling_of(headers: &HeaderMap) -> Handling {
    let declared = headers
        .get("prefer")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match declared
        .split(';')
        .chain(declared.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("handling=lenient"))
    {
        true => Handling::Lenient,
        false => Handling::Strict,
    }
}

fn unsupported() -> Response {
    let outcome = OperationOutcome::error(
        IssueCode::NotSupported,
        "this instance runs no asynchronous jobs",
    );
    let mut response = Response::new(Body::from(outcome.to_fhir_json()));
    *response.status_mut() = StatusCode::NOT_IMPLEMENTED;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
    response
}

fn host_of(headers: &HeaderMap) -> String {
    let held = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost")
        .trim();
    match held.is_empty() {
        true => "localhost".to_owned(),
        false => held.to_owned(),
    }
}

pub fn status_location(host: &str, id: &JobId) -> String {
    format!("http://{host}{JOBS}/{id}")
}

fn progress_of(record: &JobRecord) -> String {
    let mut reported = record.state.as_str().to_owned();
    if let (Some(total), Some(percent)) = (record.progress.total, record.progress.percent()) {
        reported.push_str(&format!(" {}/{total} {percent}%", record.progress.done));
    }
    if let Some(detail) = &record.progress.detail {
        reported.push(' ');
        reported.push_str(detail);
    }
    reported
}

fn submitted_as(record: &JobRecord) -> Value {
    record
        .payload
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(Value::Null)
}

fn manifest(host: &str, record: &JobRecord, files: &[fhir_store::Output]) -> Value {
    let outcome = record
        .outcome
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| Value::String(record.outcome.clone().unwrap_or_default()));
    let entry = |file: &fhir_store::Output| {
        serde_json::json!({
            "type": file.kind,
            "url": format!("http://{host}{JOBS}/{}/{}", record.id, file.name),
            "count": file.count,
        })
    };
    let listed = |wanted: bool| -> Vec<Value> {
        files
            .iter()
            .filter(|file| (file.kind == OUTCOME) == wanted)
            .map(entry)
            .collect()
    };
    let mut body = serde_json::json!({
        "id": record.id.as_str(),
        "kind": record.kind.as_str(),
        "state": record.state.as_str(),
        "progress": {
            "done": record.progress.done,
            "total": record.progress.total,
            "detail": record.progress.detail,
        },
        "request": submitted_as(record),
        "requiresAccessToken": false,
        "output": listed(false),
        "error": listed(true),
        "outcome": outcome,
    });
    if let Some(instant) = record
        .outcome
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|found| found.get("transactionTime").cloned())
    {
        body["transactionTime"] = instant;
    }
    body
}

fn action_of(kind: JobKind) -> DataAction {
    match kind {
        JobKind::Import => DataAction::Import,
        JobKind::Export => DataAction::Export,
        JobKind::BulkDelete => DataAction::BulkDelete,
        JobKind::BulkUpdate => DataAction::BulkUpdate,
        JobKind::Reindex => DataAction::Reindex,
        JobKind::Interaction => DataAction::Read,
    }
}

fn accepted(host: &str, id: &JobId) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::ACCEPTED;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_LOCATION,
        HeaderValue::from_str(&status_location(host, id)).expect("a status location is a header"),
    );
    headers.insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&RETRY_AFTER.to_string()).expect("a retry delay is a header"),
    );
    response
}

async fn submit(state: &AppState, kind: JobKind, headers: &HeaderMap, body: &[u8]) -> Response {
    let access = match crate::handlers::allowed(state, headers, action_of(kind), None, None).await {
        Ok(access) => access,
        Err(error) => return AppError::from(error).into_response_now(),
    };
    if let Err(error) = wide_enough(state, &access, headers, action_of(kind)) {
        return AppError::from(error).into_response_now();
    }
    let Some(jobs) = queue(state) else {
        return unsupported();
    };
    let payload = match std::str::from_utf8(body) {
        Ok(text) if !text.trim().is_empty() => text.to_owned(),
        Ok(_) => "{}".to_owned(),
        Err(error) => {
            return AppError::from(Error::InvalidJson(error.to_string())).into_response_now()
        }
    };
    let id = match JobId::parse(&Uuid::new_v4().to_string()) {
        Ok(id) => id,
        Err(error) => return AppError::from(error).into_response_now(),
    };
    let request = match access.secured {
        true => JobRequest::new(id.clone(), kind, payload).owned_by(&access.actor),
        false => JobRequest::new(id.clone(), kind, payload),
    }
    .correlated(crate::measure::correlation_of(headers));
    match jobs.submit(request).await {
        Ok(_) => accepted(&host_of(headers), &id),
        Err(error) => AppError::from(error).into_response_now(),
    }
}

pub(crate) async fn deferred(
    state: &AppState,
    asked: &crate::interaction::Asked,
    headers: &HeaderMap,
) -> Option<Response> {
    match crate::preference::respond_async(headers) {
        false => None,
        true => Some(
            submit(
                state,
                JobKind::Interaction,
                headers,
                asked.to_request().as_bytes(),
            )
            .await,
        ),
    }
}

pub async fn submit_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_export_scope(&state, "system", None, &headers, query.as_deref(), &body).await
}

pub async fn submit_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    submit(&state, JobKind::Import, &headers, &body).await
}

pub async fn submit_bulk_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_removal(&state, None, false, &headers, query.as_deref(), &body).await
}

pub async fn submit_bulk_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_patching(&state, None, &headers, query.as_deref(), &body).await
}

pub async fn submit_type_bulk_update(
    State(state): State<AppState>,
    Path(resource_type): Path<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_patching(
        &state,
        Some(&resource_type),
        &headers,
        query.as_deref(),
        &body,
    )
    .await
}

pub async fn submit_reindex(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_indexing(&state, None, &headers, query.as_deref(), &body).await
}

pub async fn submit_resource_reindex(
    State(state): State<AppState>,
    Path((resource_type, id)): Path<(String, String)>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    let reference = format!("{resource_type}/{id}");
    submit_indexing(&state, Some(&reference), &headers, query.as_deref(), &body).await
}

fn indexing(reference: Option<&str>, raw: Option<&str>, body: &[u8]) -> Result<String, Error> {
    let (mut carried, _) = merged(
        raw,
        body,
        &REINDEX_PARAMS,
        &REINDEX_LISTED,
        Handling::Strict,
    )?;
    if let Some(reference) = reference {
        carried.insert(
            "_resource".to_owned(),
            Value::Array(vec![Value::String(reference.to_owned())]),
        );
    }
    Ok(Value::Object(carried).to_string())
}

async fn submit_indexing(
    state: &AppState,
    reference: Option<&str>,
    headers: &HeaderMap,
    raw: Option<&str>,
    body: &[u8],
) -> Response {
    match indexing(reference, raw, body) {
        Ok(payload) => submit(state, JobKind::Reindex, headers, payload.as_bytes()).await,
        Err(error) => AppError::from(error).into_response_now(),
    }
}

pub async fn poll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_text): Path<String>,
) -> Response {
    let access =
        match crate::handlers::allowed(&state, &headers, DataAction::Read, None, None).await {
            Ok(access) => access,
            Err(error) => return AppError::from(error).into_response_now(),
        };
    let Some(jobs) = queue(&state) else {
        return unsupported();
    };
    let id = match JobId::parse(&id_text) {
        Ok(id) => id,
        Err(_) => return AppError::from(Error::NotFound).into_response_now(),
    };
    let record = match jobs.fetch(&id).await {
        Ok(record) => record,
        Err(error) => return AppError::from(error).into_response_now(),
    };
    if let Err(error) = owns(&access, &record) {
        return AppError::from(error).into_response_now();
    }
    match record.state {
        JobState::Queued | JobState::Running | JobState::Cancelling => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::ACCEPTED;
            let headers = response.headers_mut();
            headers.insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&RETRY_AFTER.to_string()).expect("a retry delay is a header"),
            );
            headers.insert(
                "x-progress",
                HeaderValue::from_str(&progress_of(&record)).expect("progress is a header"),
            );
            response
        }
        JobState::Completed if record.kind == JobKind::Interaction => {
            let body = record
                .outcome
                .as_deref()
                .and_then(InteractionEntry::read_back)
                .unwrap_or(Value::Null);
            let mut response = Response::new(Body::from(body.to_string()));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
            response
        }
        JobState::Completed => {
            let files = match sink(&state) {
                Some(sink) => sink.list(&record.id).await.unwrap_or_default(),
                None => Vec::new(),
            };
            let body = manifest(&host_of(&headers), &record, &files).to_string();
            let mut response = Response::new(Body::from(body));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON));
            response
        }
        JobState::Failed => AppError::from(Error::Internal(
            record
                .outcome
                .unwrap_or_else(|| "the job failed".to_owned()),
        ))
        .into_response_now(),
        JobState::Cancelled => AppError::from(Error::NotFound).into_response_now(),
    }
}

pub async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_text): Path<String>,
) -> Response {
    let access =
        match crate::handlers::allowed(&state, &headers, DataAction::Read, None, None).await {
            Ok(access) => access,
            Err(error) => return AppError::from(error).into_response_now(),
        };
    let Some(jobs) = queue(&state) else {
        return unsupported();
    };
    let id = match JobId::parse(&id_text) {
        Ok(id) => id,
        Err(_) => return AppError::from(Error::NotFound).into_response_now(),
    };
    match jobs
        .fetch(&id)
        .await
        .and_then(|record| owns(&access, &record))
    {
        Ok(()) => {}
        Err(error) => return AppError::from(error).into_response_now(),
    }
    match jobs.cancel(&id).await {
        Ok(_) => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::ACCEPTED;
            response
        }
        Err(error) => AppError::from(error).into_response_now(),
    }
}

pub async fn output(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id_text, name)): Path<(String, String)>,
) -> Response {
    let access =
        match crate::handlers::allowed(&state, &headers, DataAction::Read, None, None).await {
            Ok(access) => access,
            Err(error) => return AppError::from(error).into_response_now(),
        };
    let Some(sink) = sink(&state) else {
        return unsupported();
    };
    let id = match JobId::parse(&id_text) {
        Ok(id) => id,
        Err(_) => return AppError::from(Error::NotFound).into_response_now(),
    };
    if access.secured {
        let owned = match queue(&state) {
            None => Err(Error::NotFound),
            Some(jobs) => jobs
                .fetch(&id)
                .await
                .and_then(|record| owns(&access, &record)),
        };
        if let Err(error) = owned {
            return AppError::from(error).into_response_now();
        }
    }
    match sink.read(&id, &name).await {
        Ok(body) => {
            let mut response = Response::new(Body::from(body));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(NDJSON));
            response
        }
        Err(error) => AppError::from(error).into_response_now(),
    }
}

fn merged(
    raw: Option<&str>,
    body: &[u8],
    accepted: &[&str],
    listed: &[&str],
    handling: Handling,
) -> Result<(serde_json::Map<String, Value>, Vec<String>), Error> {
    let mut payload = match std::str::from_utf8(body) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(text)
            .map_err(|error| Error::InvalidJson(error.to_string()))?,
        Ok(_) => Value::Object(serde_json::Map::new()),
        Err(error) => return Err(Error::InvalidJson(error.to_string())),
    };
    let carried = payload
        .as_object_mut()
        .ok_or_else(|| Error::InvalidJson("a request is described by an object".to_owned()))?;
    let mut dropped = Vec::new();
    for (name, value) in crate::query::pairs(raw) {
        let refused = match () {
            _ if !accepted.contains(&name.as_str()) => {
                Some(Error::UnsupportedParameter(format!("{name:?}")))
            }
            _ if value.trim().is_empty() => Some(Error::UnsupportedParameter(format!(
                "{name:?} with an empty value"
            ))),
            _ if name == "_outputFormat" => fhir_store::output_format(&value).err(),
            _ => None,
        };
        if let Some(error) = refused {
            match handling {
                Handling::Lenient => {
                    dropped.push(name);
                    continue;
                }
                Handling::Strict => return Err(error),
            }
        }
        if name == "includeAssociatedData" {
            let parts = value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<&str>>();
            let carried_parts = parts
                .iter()
                .copied()
                .filter(|part| {
                    matches!(
                        *part,
                        "LatestProvenanceResources" | "RelevantProvenanceResources"
                    )
                })
                .collect::<Vec<&str>>();
            if carried_parts.len() != parts.len() {
                match handling {
                    Handling::Lenient if carried_parts.is_empty() => {
                        dropped.push(name);
                        continue;
                    }
                    Handling::Lenient => {}
                    Handling::Strict => {
                        let unknown = parts
                            .iter()
                            .find(|part| !carried_parts.contains(part))
                            .copied()
                            .unwrap_or_default();
                        return Err(Error::UnsupportedParameter(format!(
                            "includeAssociatedData {unknown:?} is not carried"
                        )));
                    }
                }
            }
            if carried_parts.len() != parts.len() {
                let items = carried_parts
                    .iter()
                    .map(|part| Value::String((*part).to_owned()))
                    .collect();
                carried.insert(name, Value::Array(items));
                continue;
            }
        }
        match listed.contains(&name.as_str()) {
            true => {
                let items = value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(|part| Value::String(part.to_owned()))
                    .collect();
                carried.insert(name, Value::Array(items))
            }
            false => carried.insert(name, Value::String(value)),
        };
    }
    Ok((carried.clone(), dropped))
}

fn described(
    scope: &str,
    id: Option<&str>,
    raw: Option<&str>,
    body: &[u8],
    headers: &HeaderMap,
) -> Result<String, Error> {
    let (mut carried, dropped) =
        merged(raw, body, &ACCEPTED_PARAMS, &LISTED, handling_of(headers))?;
    carried.insert("scope".to_owned(), Value::String(scope.to_owned()));
    if let Some(id) = id {
        carried.insert("id".to_owned(), Value::String(id.to_owned()));
    }
    if !carried.contains_key("_till") && !carried.contains_key("_until") {
        carried.insert(
            "_till".to_owned(),
            Value::String(fhir_store::system_clock()().as_str().to_owned()),
        );
    }
    if !dropped.is_empty() {
        carried.insert(
            "_unsupported".to_owned(),
            Value::Array(dropped.into_iter().map(Value::String).collect()),
        );
    }
    Ok(Value::Object(carried).to_string())
}

fn removal(
    resource_type: Option<&str>,
    soft_deleted: bool,
    raw: Option<&str>,
    body: &[u8],
) -> Result<String, Error> {
    let (mut carried, _) = merged(raw, body, &DELETE_PARAMS, &DELETE_LISTED, Handling::Strict)?;
    if let Some(resource_type) = resource_type {
        carried.insert(
            "_type".to_owned(),
            Value::Array(vec![Value::String(resource_type.to_owned())]),
        );
    }
    carried.insert("softDeleted".to_owned(), Value::Bool(soft_deleted));
    Ok(Value::Object(carried).to_string())
}

fn supplied(body: &[u8]) -> Result<Value, Error> {
    match std::str::from_utf8(body) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str(text).map_err(|error| Error::InvalidJson(error.to_string()))
        }
        Ok(_) => Ok(Value::Null),
        Err(error) => Err(Error::InvalidJson(error.to_string())),
    }
}

fn patching(resource_type: Option<&str>, raw: Option<&str>, body: &[u8]) -> Result<String, Error> {
    let held = supplied(body)?;
    let describes = matches!(&held, Value::Object(map) if map.contains_key("patch"));
    let mut carried = match describes {
        true => merged(raw, body, &UPDATE_PARAMS, &DELETE_LISTED, Handling::Strict)?.0,
        false => merged(raw, b"", &UPDATE_PARAMS, &DELETE_LISTED, Handling::Strict)?.0,
    };
    if !describes && !held.is_null() {
        carried.insert("patch".to_owned(), held);
    }
    if let Some(resource_type) = resource_type {
        carried.insert(
            "_type".to_owned(),
            Value::Array(vec![Value::String(resource_type.to_owned())]),
        );
    }
    Ok(Value::Object(carried).to_string())
}

async fn submit_patching(
    state: &AppState,
    resource_type: Option<&str>,
    headers: &HeaderMap,
    raw: Option<&str>,
    body: &[u8],
) -> Response {
    match patching(resource_type, raw, body) {
        Ok(payload) => submit(state, JobKind::BulkUpdate, headers, payload.as_bytes()).await,
        Err(error) => AppError::from(error).into_response_now(),
    }
}

async fn submit_removal(
    state: &AppState,
    resource_type: Option<&str>,
    soft_deleted: bool,
    headers: &HeaderMap,
    raw: Option<&str>,
    body: &[u8],
) -> Response {
    match removal(resource_type, soft_deleted, raw, body) {
        Ok(payload) => submit(state, JobKind::BulkDelete, headers, payload.as_bytes()).await,
        Err(error) => AppError::from(error).into_response_now(),
    }
}

pub async fn submit_type_bulk_delete(
    State(state): State<AppState>,
    Path(resource_type): Path<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_removal(
        &state,
        Some(&resource_type),
        false,
        &headers,
        query.as_deref(),
        &body,
    )
    .await
}

pub async fn submit_bulk_delete_soft_deleted(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_removal(&state, None, true, &headers, query.as_deref(), &body).await
}

pub async fn submit_type_bulk_delete_soft_deleted(
    State(state): State<AppState>,
    Path(resource_type): Path<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_removal(
        &state,
        Some(&resource_type),
        true,
        &headers,
        query.as_deref(),
        &body,
    )
    .await
}

async fn submit_export_scope(
    state: &AppState,
    scope: &str,
    id: Option<&str>,
    headers: &HeaderMap,
    raw: Option<&str>,
    body: &[u8],
) -> Response {
    match described(scope, id, raw, body, headers) {
        Ok(payload) => submit(state, JobKind::Export, headers, payload.as_bytes()).await,
        Err(error) => AppError::from(error).into_response_now(),
    }
}

pub async fn submit_patient_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_export_scope(&state, "patient", None, &headers, query.as_deref(), &body).await
}

pub async fn submit_group_export(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: axum::body::Bytes,
) -> Response {
    submit_export_scope(
        &state,
        "group",
        Some(&id),
        &headers,
        query.as_deref(),
        &body,
    )
    .await
}

fn wide_enough(
    state: &AppState,
    access: &Access,
    headers: &HeaderMap,
    action: DataAction,
) -> Result<(), Error> {
    let Some(grant) = crate::handlers::confining(state, access, headers, action)? else {
        return Ok(());
    };
    match grant.is_open() && grant.filters.is_empty() {
        true => Ok(()),
        false => Err(Error::Forbidden(format!(
            "{} under a confined grant",
            action.as_str()
        ))),
    }
}

fn owns(access: &Access, record: &JobRecord) -> Result<(), Error> {
    match !access.secured || record.owner.as_deref() == Some(access.actor.as_str()) {
        true => Ok(()),
        false => Err(Error::NotFound),
    }
}
