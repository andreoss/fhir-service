use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fhir_core::{Error, IssueCode, OperationOutcome};
use std::sync::Arc;
use tokio::sync::Semaphore;

const FHIR_JSON: &str = "application/fhir+json";

const EXEMPT: [&str; 4] = [
    "/health",
    "/metadata",
    "/.well-known/smart-configuration",
    crate::scrape::METRICS,
];

const RETRY_AFTER: u64 = 1;

#[derive(Debug, Clone, Default)]
pub struct Throttle {
    permits: Option<Arc<Semaphore>>,
    at_once: usize,
}

impl PartialEq for Throttle {
    fn eq(&self, other: &Throttle) -> bool {
        self.at_once == other.at_once && self.is_bounded() == other.is_bounded()
    }
}

impl Eq for Throttle {}

impl Throttle {
    pub fn unbounded() -> Throttle {
        Throttle::default()
    }

    pub fn of(at_once: usize) -> Result<Throttle, Error> {
        if at_once == 0 {
            return Err(Error::Config(
                "a load bound of zero would refuse every request".to_owned(),
            ));
        }
        Ok(Throttle {
            permits: Some(Arc::new(Semaphore::new(at_once))),
            at_once,
        })
    }

    pub fn is_bounded(&self) -> bool {
        self.permits.is_some()
    }

    pub fn at_once(&self) -> usize {
        self.at_once
    }
}

pub async fn bounded(
    State(state): State<crate::app::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(permits) = state.throttle.permits.clone() else {
        return next.run(request).await;
    };
    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_default();
    if EXEMPT.contains(&path.as_str()) {
        return next.run(request).await;
    }
    let Ok(held) = permits.try_acquire_owned() else {
        return refused(state.throttle.at_once);
    };
    let response = next.run(request).await;
    drop(held);
    response
}

fn refused(at_once: usize) -> Response {
    let outcome = OperationOutcome::error(
        IssueCode::Throttled,
        format!(
            "this instance carries {at_once} requests at once and is already carrying them; \
             ask again"
        ),
    );
    let mut response = (StatusCode::TOO_MANY_REQUESTS, outcome.to_fhir_json()).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
    headers.insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&RETRY_AFTER.to_string()).expect("a retry-after is a header value"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_configured_bounds_nothing() {
        assert!(!Throttle::unbounded().is_bounded());
    }

    #[test]
    fn a_bound_of_zero_is_refused() {
        assert!(Throttle::of(0).is_err());
    }

    #[test]
    fn a_bound_is_carried() {
        let held = Throttle::of(4).unwrap();
        assert!(held.is_bounded());
        assert_eq!(held.at_once(), 4);
    }

    #[test]
    fn the_routes_that_report_on_the_instance_are_exempt() {
        for path in ["/health", "/metadata"] {
            assert!(EXEMPT.contains(&path), "{path}");
        }
    }

    #[test]
    fn a_refusal_asks_the_client_to_wait() {
        let response = refused(4);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
    }
}
