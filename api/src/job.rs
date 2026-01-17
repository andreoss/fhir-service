use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::StatusCode;
use axum::response::Response;
use fhir_core::{IssueCode, OperationOutcome};
use fhir_core::Error;
use fhir_store::{JobId, JobKind, JobRecord, JobRequest, JobState, JobStore};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use crate::app::AppState;
use crate::handlers::AppError;

const FHIR_JSON: &str = "application/fhir+json";
const JSON: &str = "application/json";

const NDJSON: &str = "application/fhir+ndjson";

const OUTCOME: &str = "OperationOutcome";

const LISTED: [&str; 2] = ["_type", "_typeFilter"];

pub const JOBS: &str = "/_jobs";

pub const RETRY_AFTER: u64 = 1;

fn queue(state: &AppState) -> Option<Arc<dyn JobStore>> {
    state.jobs.as_ref().map(Arc::clone)
}

fn sink(state: &AppState) -> Option<Arc<dyn fhir_store::BulkStore>> {
    state.outputs.as_ref().map(Arc::clone)
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
    match record.progress.percent() {
        Some(percent) => format!("{} {percent}%", record.state.as_str()),
        None => record.state.as_str().to_owned(),
    }
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
        },
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
    match jobs.submit(JobRequest::new(id.clone(), kind, payload)).await {
        Ok(_) => accepted(&host_of(headers), &id),
        Err(error) => AppError::from(error).into_response_now(),
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
    body: axum::body::Bytes,
) -> Response {
    submit(&state, JobKind::BulkDelete, &headers, &body).await
}

pub async fn submit_bulk_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    submit(&state, JobKind::BulkUpdate, &headers, &body).await
}

pub async fn submit_reindex(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    submit(&state, JobKind::Reindex, &headers, &body).await
}

pub async fn poll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_text): Path<String>,
) -> Response {
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
            record.outcome.unwrap_or_else(|| "the job failed".to_owned()),
        ))
        .into_response_now(),
        JobState::Cancelled => AppError::from(Error::NotFound).into_response_now(),
    }
}

pub async fn cancel(State(state): State<AppState>, Path(id_text): Path<String>) -> Response {
    let Some(jobs) = queue(&state) else {
        return unsupported();
    };
    let id = match JobId::parse(&id_text) {
        Ok(id) => id,
        Err(_) => return AppError::from(Error::NotFound).into_response_now(),
    };
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
    Path((id_text, name)): Path<(String, String)>,
) -> Response {
    let Some(sink) = sink(&state) else {
        return unsupported();
    };
    let id = match JobId::parse(&id_text) {
        Ok(id) => id,
        Err(_) => return AppError::from(Error::NotFound).into_response_now(),
    };
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

fn described(
    scope: &str,
    id: Option<&str>,
    raw: Option<&str>,
    body: &[u8],
) -> Result<String, Error> {
    let mut payload = match std::str::from_utf8(body) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(text)
            .map_err(|error| Error::InvalidJson(error.to_string()))?,
        Ok(_) => Value::Object(serde_json::Map::new()),
        Err(error) => return Err(Error::InvalidJson(error.to_string())),
    };
    let carried = payload
        .as_object_mut()
        .ok_or_else(|| Error::InvalidJson("an export is described by an object".to_owned()))?;
    carried.insert("scope".to_owned(), Value::String(scope.to_owned()));
    if let Some(id) = id {
        carried.insert("id".to_owned(), Value::String(id.to_owned()));
    }
    for (name, value) in crate::query::pairs(raw) {
        if value.trim().is_empty() {
            return Err(Error::UnsupportedParameter(format!(
                "{name:?} with an empty value"
            )));
        }
        match LISTED.contains(&name.as_str()) {
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
    if !carried.contains_key("_till") {
        carried.insert(
            "_till".to_owned(),
            Value::String(fhir_store::system_clock()().as_str().to_owned()),
        );
    }
    Ok(Value::Object(carried.clone()).to_string())
}

async fn submit_export_scope(
    state: &AppState,
    scope: &str,
    id: Option<&str>,
    headers: &HeaderMap,
    raw: Option<&str>,
    body: &[u8],
) -> Response {
    match described(scope, id, raw, body) {
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
