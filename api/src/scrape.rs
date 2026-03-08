use axum::extract::State;
use axum::http::header::{self, HeaderMap};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fhir_core::Error;
use fhir_telemetry::Admission;

use crate::app::AppState;
use crate::handlers::AppError;

pub const METRICS: &str = "/_metrics";

const EXPOSITION: &str = "text/plain; version=0.0.4; charset=utf-8";
const BEARER: &str = "Bearer ";

pub fn offered(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|held| held.to_str().ok())
        .and_then(|held| held.strip_prefix(BEARER))
        .map(|held| held.trim())
}

pub async fn metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    match state.scrape.admits(offered(&headers)) {
        Admission::Unserved => Err(Error::NotFound.into()),
        Admission::Refused => {
            Err(Error::Forbidden("the measurements are restricted".to_owned()).into())
        }
        Admission::Granted => Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, EXPOSITION),
                (header::CACHE_CONTROL, "no-store"),
            ],
            state.telemetry.exposition(),
        )
            .into_response()),
    }
}
