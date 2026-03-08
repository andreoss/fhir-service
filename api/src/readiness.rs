







use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use fhir_core::{IssueCode, OperationOutcome};
use std::sync::{Arc, RwLock};

const FHIR_JSON: &str = "application/fhir+json";




const LOCKED: u16 = 423;



#[derive(Debug, Clone, Default)]
pub struct Busy {
    held: Arc<RwLock<Option<String>>>,
}

impl Busy {
    pub fn new() -> Busy {
        Busy::default()
    }

    
    pub fn during(&self, what: &str) -> Held {
        if let Ok(mut held) = self.held.write() {
            *held = Some(what.to_owned());
        }
        Held { busy: self.clone() }
    }

    pub fn holding(&self) -> Option<String> {
        self.held.read().ok().and_then(|held| held.clone())
    }

    fn release(&self) {
        if let Ok(mut held) = self.held.write() {
            *held = None;
        }
    }
}



pub struct Held {
    busy: Busy,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.busy.release();
    }
}

pub async fn liveness() -> Response {
    answered(StatusCode::OK, "this instance is up")
}

pub async fn readiness(State(state): State<crate::app::AppState>) -> Response {
    match state.busy.holding() {
        None => answered(StatusCode::OK, "this instance is ready"),
        Some(what) => answered(
            StatusCode::from_u16(LOCKED).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            &format!("this instance is up but busy with {what}; it is not to be restarted"),
        ),
    }
}

fn answered(status: StatusCode, message: &str) -> Response {
    let outcome = match status.is_success() {
        true => OperationOutcome::information(message),
        false => OperationOutcome::error(IssueCode::Transient, message),
    };
    let mut response = (status, outcome.to_fhir_json()).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instance_holds_nothing_to_begin_with() {
        assert!(Busy::new().holding().is_none());
    }

    #[test]
    fn what_holds_the_instance_is_named_while_it_holds() {
        let busy = Busy::new();
        {
            let _held = busy.during("a reindex");
            assert_eq!(busy.holding().as_deref(), Some("a reindex"));
        }
        assert!(busy.holding().is_none(), "and is released after");
    }

    #[test]
    fn a_hold_that_panics_is_still_released() {
        let busy = Busy::new();
        let held = std::panic::catch_unwind({
            let busy = busy.clone();
            move || {
                let _held = busy.during("a migration");
                panic!("the work failed");
            }
        });
        assert!(held.is_err());
        assert!(busy.holding().is_none());
    }
}
