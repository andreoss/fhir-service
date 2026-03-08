use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method};
use axum::middleware::Next;
use axum::response::Response;
use fhir_core::CorrelationId;
use fhir_telemetry::{Dimensions, Operation, Outcome};
use std::time::Instant;

use crate::app::AppState;

pub fn operation_of(method: &Method, path: &str) -> Operation {
    if path.starts_with("/_jobs") {
        return Operation::Job;
    }
    if path.contains("$export") {
        return Operation::Export;
    }
    if path.contains("$import") {
        return Operation::Import;
    }
    if path.contains("$reindex") {
        return Operation::Reindex;
    }
    if path.contains("$bulk-delete") {
        return Operation::BulkDelete;
    }
    if path.contains("$bulk-update") {
        return Operation::BulkUpdate;
    }
    if path.contains("$purge-history") {
        return Operation::Delete;
    }
    if path.ends_with("/_history/{vid}") {
        return Operation::Vread;
    }
    if path.contains("_history") {
        return Operation::History;
    }
    if conformance(path) {
        return Operation::Conformance;
    }
    if path.contains('$') {
        return Operation::Other;
    }
    instance(method, path)
}

fn conformance(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/metadata" | "/.well-known/smart-configuration"
    ) || path.starts_with("/OperationDefinition")
        || path.starts_with("/CompartmentDefinition")
}

fn instance(method: &Method, path: &str) -> Operation {
    match path {
        "/" => match *method {
            Method::POST => Operation::Bundle,
            _ => Operation::Search,
        },
        "/{type}" => match *method {
            Method::POST => Operation::Create,
            Method::PUT => Operation::Update,
            Method::DELETE => Operation::Delete,
            Method::PATCH => Operation::Patch,
            _ => Operation::Search,
        },
        "/{type}/{id}" => match *method {
            Method::PUT => Operation::Update,
            Method::DELETE => Operation::Delete,
            Method::PATCH => Operation::Patch,
            _ => Operation::Read,
        },
        "/{type}/{id}/{target}" => Operation::Search,
        _ => Operation::Other,
    }
}

pub const CORRELATION: &str = "x-correlation-id";

pub fn correlation_of(headers: &axum::http::HeaderMap) -> CorrelationId {
    CorrelationId::offered(headers.get(CORRELATION).and_then(|held| held.to_str().ok()))
}

pub async fn measured(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let correlation = correlation_of(request.headers());
    if let Ok(value) = HeaderValue::from_str(correlation.as_str()) {
        request.headers_mut().insert(CORRELATION, value.clone());
        request.extensions_mut().insert(correlation.clone());
    }
    let started = Instant::now();
    let mut response = next.run(request).await;
    let millis = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let dimensions = Dimensions::of(
        operation_of(&method, &path),
        Outcome::of_status(response.status().as_u16()),
    );
    if let Ok(value) = HeaderValue::from_str(correlation.as_str()) {
        response.headers_mut().insert(CORRELATION, value);
    }
    state
        .telemetry
        .record_for(dimensions, millis, Some(correlation));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_template_names_its_operation() {
        let table = [
            (Method::GET, "/{type}/{id}", Operation::Read),
            (Method::GET, "/{type}/{id}/_history/{vid}", Operation::Vread),
            (Method::GET, "/{type}/{id}/_history", Operation::History),
            (Method::POST, "/{type}", Operation::Create),
            (Method::PUT, "/{type}/{id}", Operation::Update),
            (Method::DELETE, "/{type}/{id}", Operation::Delete),
            (Method::PATCH, "/{type}/{id}", Operation::Patch),
            (Method::GET, "/{type}", Operation::Search),
            (Method::GET, "/", Operation::Search),
            (Method::POST, "/", Operation::Bundle),
            (Method::GET, "/metadata", Operation::Conformance),
            (Method::GET, "/health", Operation::Conformance),
            (Method::POST, "/$export", Operation::Export),
            (Method::POST, "/$import", Operation::Import),
            (Method::POST, "/$reindex", Operation::Reindex),
            (Method::POST, "/{type}/{id}/$reindex", Operation::Reindex),
            (Method::POST, "/$bulk-delete", Operation::BulkDelete),
            (Method::POST, "/$bulk-update", Operation::BulkUpdate),
            (Method::GET, "/_jobs/{id}", Operation::Job),
            (Method::POST, "/$validate", Operation::Other),
            (
                Method::POST,
                "/{type}/{id}/$purge-history",
                Operation::Delete,
            ),
        ];
        for (method, path, expected) in table {
            assert_eq!(operation_of(&method, path), expected, "{path}");
        }
    }

    #[test]
    fn every_served_route_names_a_known_operation() {
        for route in crate::app::served() {
            for verb in route.methods {
                let method = Method::from_bytes(verb.as_str().as_bytes()).expect("method");
                let named = operation_of(&method, route.path);
                assert!(Operation::ALL.contains(&named), "{}", route.path);
            }
        }
    }
}
