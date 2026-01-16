use axum::body::Body;
use axum::extract::{Path, State};
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

pub const JOBS: &str = "/_jobs";

pub const RETRY_AFTER: u64 = 1;

fn queue(state: &AppState) -> Result<Arc<dyn JobStore>, Response> {
    match &state.jobs {
        Some(jobs) => Ok(Arc::clone(jobs)),
        None => Err(unsupported()),
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
    match record.progress.percent() {
        Some(percent) => format!("{} {percent}%", record.state.as_str()),
        None => record.state.as_str().to_owned(),
    }
}

fn manifest(record: &JobRecord) -> Value {
    let outcome = record
        .outcome
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| Value::String(record.outcome.clone().unwrap_or_default()));
    serde_json::json!({
        "id": record.id.as_str(),
        "kind": record.kind.as_str(),
        "state": record.state.as_str(),
        "progress": {
            "done": record.progress.done,
            "total": record.progress.total,
        },
        "outcome": outcome,
    })
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
    let jobs = match queue(state) {
        Ok(jobs) => jobs,
        Err(response) => return response,
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
    body: axum::body::Bytes,
) -> Response {
    submit(&state, JobKind::Export, &headers, &body).await
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

pub async fn poll(State(state): State<AppState>, Path(id_text): Path<String>) -> Response {
    let jobs = match queue(&state) {
        Ok(jobs) => jobs,
        Err(response) => return response,
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
            let mut response = Response::new(Body::from(manifest(&record).to_string()));
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
    let jobs = match queue(&state) {
        Ok(jobs) => jobs,
        Err(response) => return response,
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
