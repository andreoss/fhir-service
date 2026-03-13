use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use fhir_core::security::scope::DataAction;
use fhir_core::{Error, OperationOutcome};
use serde_json::Value;

use crate::app::AppState;
use crate::handlers::AppError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resettable {
    named: Option<String>,
}

impl Resettable {
    pub fn never() -> Resettable {
        Resettable::default()
    }

    pub fn named(name: &str) -> Result<Resettable, Error> {
        let held = name.trim();
        if held.is_empty() {
            return Err(Error::Config(
                "an instance that may be emptied is named, so that a caller must say which \
                 instance it meant"
                    .to_owned(),
            ));
        }
        Ok(Resettable {
            named: Some(held.to_owned()),
        })
    }

    pub fn is_allowed(&self) -> bool {
        self.named.is_some()
    }

    pub fn name(&self) -> Option<&str> {
        self.named.as_deref()
    }

    fn confirmed_by(&self, given: Option<&str>) -> bool {
        match (&self.named, given) {
            (Some(held), Some(given)) => held == given.trim(),
            _ => false,
        }
    }
}

pub async fn reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    if !state.reset.is_allowed() {
        return Err(Error::Forbidden(
            "emptying this instance, which an operator turns on deliberately and this one \
             has not"
                .to_owned(),
        )
        .into());
    }

    crate::handlers::allowed(&state, &headers, DataAction::Write, None, None).await?;
    if crate::handlers::confining(
        &state,
        &crate::access::access_of(&state, &headers).await?,
        &headers,
        DataAction::Write,
    )?
    .is_some_and(|grant| {
        !grant.types.is_empty()
            || !grant.is_open()
            || !grant.filters.is_empty()
            || !grant.every.is_empty()
    }) {
        return Err(Error::Forbidden(
            "emptying this instance, which is a write across every type".to_owned(),
        )
        .into());
    }
    let confirmation = confirmation_in(&body)?;
    if !state.reset.confirmed_by(confirmation.as_deref()) {
        return Err(Error::InvalidParameter(format!(
            "to empty this instance, name it: a \"confirm\" parameter carrying {:?}",
            state.reset.name().unwrap_or_default()
        ))
        .into());
    }
    let removed = state.store.empty().await?;
    let outcome = OperationOutcome::information(format!(
        "{removed} resources removed; this instance is as it started"
    ));
    let mut response = Response::new(axum::body::Body::from(outcome.to_fhir_json()));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/fhir+json"),
    );
    Ok(response)
}

fn confirmation_in(body: &[u8]) -> Result<Option<String>, Error> {
    if body.is_empty() {
        return Ok(None);
    }
    let held: Value =
        serde_json::from_slice(body).map_err(|error| Error::InvalidJson(error.to_string()))?;
    Ok(crate::operation::value_of(&held, "confirm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instance_says_nothing_and_cannot_be_emptied() {
        assert!(!Resettable::never().is_allowed());
        assert!(!Resettable::never().confirmed_by(Some("anything")));
    }

    #[test]
    fn an_instance_with_no_name_is_refused_at_startup() {
        assert!(Resettable::named("  ").is_err());
    }

    #[test]
    fn only_the_name_it_was_given_confirms() {
        let held = Resettable::named("staging").expect("a name");
        assert!(held.confirmed_by(Some("staging")));
        assert!(held.confirmed_by(Some(" staging ")));
        assert!(!held.confirmed_by(Some("production")));
        assert!(!held.confirmed_by(None));
    }
}
