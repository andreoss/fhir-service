use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use fhir_core::{Error, FhirVersion, IssueCode, OperationOutcome};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

use crate::app::Service;

pub struct Endpoints {
    default: FhirVersion,
    held: Arc<Vec<(FhirVersion, Router)>>,
}

impl Endpoints {
    pub fn new(
        default: FhirVersion,
        services: Vec<(FhirVersion, Service)>,
    ) -> Result<Endpoints, Error> {
        if services.is_empty() {
            return Err(Error::Config(
                "an instance serves at least one release".to_owned(),
            ));
        }
        if !services.iter().any(|(version, _)| *version == default) {
            return Err(Error::Config(format!(
                "{default} is the default release but is not among those served"
            )));
        }
        let mut seen = Vec::new();
        for (version, _) in &services {
            if seen.contains(version) {
                return Err(Error::Config(format!("{version} is served twice")));
            }
            seen.push(*version);
        }
        Ok(Endpoints {
            default,
            held: Arc::new(
                services
                    .into_iter()
                    .map(|(version, service)| (version, service.router()))
                    .collect(),
            ),
        })
    }

    pub fn default_version(&self) -> FhirVersion {
        self.default
    }

    pub fn served(&self) -> Vec<FhirVersion> {
        self.held.iter().map(|(version, _)| *version).collect()
    }

    pub fn router(self) -> Router {
        let default = self.default;
        let held = Arc::clone(&self.held);
        let versions = self.served();
        Router::new()
            .route(
                "/$versions",
                axum::routing::get({
                    let versions = versions.clone();
                    move || {
                        let versions = versions.clone();
                        async move { reported(default, &versions) }
                    }
                })
                .post({
                    let versions = versions.clone();
                    move || {
                        let versions = versions.clone();
                        async move { reported(default, &versions) }
                    }
                }),
            )
            .fallback(move |request: Request<Body>| {
                let held = Arc::clone(&held);
                async move { dispatched(default, held, request).await }
            })
    }
}

fn reported(default: FhirVersion, served: &[FhirVersion]) -> Response {
    let mut parameters: Vec<serde_json::Value> = served
        .iter()
        .map(|version| json!({"name": "version", "valueString": version.release()}))
        .collect();
    parameters.push(json!({"name": "default", "valueString": default.release()}));
    let body = json!({"resourceType": "Parameters", "parameter": parameters});
    crate::handlers::rendered(
        serde_json::to_vec(&body).expect("a parameters resource is serializable"),
    )
}

async fn dispatched(
    default: FhirVersion,
    held: Arc<Vec<(FhirVersion, Router)>>,
    request: Request<Body>,
) -> Response {
    let (asked, path) = match named_in_path(request.uri().path()) {
        Some((version, rest)) => (Some(version), Some(rest)),
        None => (named_in_headers(request.headers()), None),
    };
    let wanted = asked.unwrap_or(default);
    let Some((_, router)) = held.iter().find(|(version, _)| *version == wanted) else {
        return refused(wanted, &held);
    };
    let mut request = request;
    if let Some(rest) = path {
        let query = request
            .uri()
            .query()
            .map(|held| format!("?{held}"))
            .unwrap_or_default();
        if let Ok(uri) = format!("{rest}{query}").parse::<Uri>() {
            *request.uri_mut() = uri;
        }
    }
    match router.clone().oneshot(request).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            OperationOutcome::error(IssueCode::Processing, "the request was not dispatched")
                .to_fhir_json(),
        )
            .into_response(),
    }
}

fn refused(wanted: FhirVersion, held: &[(FhirVersion, Router)]) -> Response {
    let served: Vec<&str> = held.iter().map(|(version, _)| version.as_str()).collect();
    let outcome = OperationOutcome::error(
        IssueCode::NotSupported,
        format!(
            "this instance serves {}, and does not serve {wanted}",
            served.join(", ")
        ),
    );
    (StatusCode::NOT_FOUND, outcome.to_fhir_json()).into_response()
}

fn named_in_path(path: &str) -> Option<(FhirVersion, String)> {
    let rest = path.strip_prefix('/')?;
    let (head, tail) = match rest.split_once('/') {
        Some((head, tail)) => (head, format!("/{tail}")),
        None => (rest, "/".to_owned()),
    };
    let version = FhirVersion::ALL
        .into_iter()
        .find(|held| held.as_str().eq_ignore_ascii_case(head))?;
    Some((version, tail))
}

fn named_in_headers(headers: &HeaderMap) -> Option<FhirVersion> {
    for name in [header::ACCEPT, header::CONTENT_TYPE] {
        let Some(value) = headers.get(name).and_then(|held| held.to_str().ok()) else {
            continue;
        };
        for piece in value.split([',', ';']) {
            let Some((key, held)) = piece.split_once('=') else {
                continue;
            };
            if !key.trim().eq_ignore_ascii_case("fhirversion") {
                continue;
            }
            let held = held.trim().trim_matches('"');
            if let Some(version) = read_release(held) {
                return Some(version);
            }
        }
    }
    None
}

fn read_release(held: &str) -> Option<FhirVersion> {
    match held {
        "3.0" => Some(FhirVersion::Stu3),
        "4.0" => Some(FhirVersion::R4),
        "4.3" => Some(FhirVersion::R4b),
        "5.0" => Some(FhirVersion::R5),
        other => other.parse::<FhirVersion>().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_that_begins_with_a_release_names_it() {
        let (version, rest) = named_in_path("/R4/Patient/1").unwrap();
        assert_eq!(version, FhirVersion::R4);
        assert_eq!(rest, "/Patient/1");
        let (version, rest) = named_in_path("/STU3").unwrap();
        assert_eq!(version, FhirVersion::Stu3);
        assert_eq!(rest, "/");
        assert_eq!(named_in_path("/r4b/metadata").unwrap().0, FhirVersion::R4b);
    }

    #[test]
    fn a_path_that_does_not_begin_with_one_names_none() {
        assert!(named_in_path("/Patient/1").is_none());
        assert!(named_in_path("/metadata").is_none());
        assert!(named_in_path("/").is_none());
    }

    #[test]
    fn a_media_type_may_carry_the_release() {
        let mut held = HeaderMap::new();
        held.insert(
            header::ACCEPT,
            "application/fhir+json; fhirVersion=4.0".parse().unwrap(),
        );
        assert_eq!(named_in_headers(&held), Some(FhirVersion::R4));
        held.insert(header::ACCEPT, "application/fhir+json".parse().unwrap());
        assert_eq!(named_in_headers(&held), None);
    }

    #[test]
    fn every_release_has_a_media_type_spelling() {
        for (held, expected) in [
            ("3.0", FhirVersion::Stu3),
            ("4.0", FhirVersion::R4),
            ("4.3", FhirVersion::R4b),
            ("5.0", FhirVersion::R5),
        ] {
            assert_eq!(read_release(held), Some(expected), "{held}");
        }
    }
}
