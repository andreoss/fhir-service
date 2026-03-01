use fhir_core::security::Access;
use fhir_store::{JobId, Ticker};
use std::collections::HashMap;
use std::sync::Mutex;

use axum::http::HeaderMap;

pub const KEPT: usize = 4_096;

const WAIT_MS: i64 = (crate::job::RETRY_AFTER as i64) * 1_000;

pub struct Polling {
    ticks: Ticker,
    seen: Mutex<HashMap<Asked, i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Asked {
    client: String,
    job: JobId,
}

impl Polling {
    pub fn new(ticks: Ticker) -> Polling {
        Polling {
            ticks,
            seen: Mutex::new(HashMap::new()),
        }
    }

    pub fn asked(&self, client: &str, job: &JobId) -> Result<(), u32> {
        let now = (self.ticks)();
        let mut held = self
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if held.len() >= KEPT {
            held.retain(|_, at| now - *at < WAIT_MS);
        }
        if held.len() >= KEPT {
            held.clear();
        }
        let previous = held.insert(
            Asked {
                client: client.to_owned(),
                job: job.clone(),
            },
            now,
        );
        match previous {
            Some(at) if now - at < WAIT_MS => Err(waiting((WAIT_MS - (now - at)) as u64)),
            _ => Ok(()),
        }
    }

    pub fn kept(&self) -> usize {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

fn waiting(left: u64) -> u32 {
    (left.div_ceil(1_000)).max(1) as u32
}

pub fn client_of(access: &Access, headers: &HeaderMap) -> String {
    match access.secured {
        true => access
            .client
            .clone()
            .unwrap_or_else(|| access.actor.clone()),
        false => headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|hop| !hop.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| access.actor.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_store::StepTicker;

    fn ticker() -> StepTicker {
        StepTicker::starting_at(1_000)
    }

    fn job(name: &str) -> JobId {
        JobId::parse(name).expect("a job id is valid")
    }

    #[test]
    fn the_first_query_of_a_client_is_not_held_back() {
        let polling = Polling::new(ticker().ticker());
        assert!(polling.asked("one", &job("j1")).is_ok());
    }

    #[test]
    fn a_query_inside_the_wait_is_answered_with_the_wait() {
        let polling = Polling::new(ticker().ticker());
        assert!(polling.asked("one", &job("j1")).is_ok());
        assert_eq!(polling.asked("one", &job("j1")), Err(1));
    }

    #[test]
    fn a_query_after_the_wait_is_not_held_back() {
        let ticks = ticker();
        let polling = Polling::new(ticks.ticker());
        assert!(polling.asked("one", &job("j1")).is_ok());
        ticks.advance(WAIT_MS);
        assert!(polling.asked("one", &job("j1")).is_ok());
    }

    #[test]
    fn one_client_is_counted_apart_from_another() {
        let polling = Polling::new(ticker().ticker());
        assert!(polling.asked("one", &job("j1")).is_ok());
        assert!(polling.asked("two", &job("j1")).is_ok());
    }

    #[test]
    fn one_job_is_counted_apart_from_another() {
        let polling = Polling::new(ticker().ticker());
        assert!(polling.asked("one", &job("j1")).is_ok());
        assert!(polling.asked("one", &job("j2")).is_ok());
    }

    #[test]
    fn what_is_kept_never_grows_past_the_bound() {
        let polling = Polling::new(ticker().ticker());
        for index in 0..(KEPT * 2) {
            let _ = polling.asked("one", &job(&format!("j{index}")));
        }
        assert!(polling.kept() <= KEPT, "kept {}", polling.kept());
    }

    #[test]
    fn a_client_the_request_names_is_the_client_counted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1".parse().unwrap());
        let open = Access::open();
        assert_eq!(client_of(&open, &headers), "10.0.0.1");
        assert_eq!(client_of(&open, &HeaderMap::new()), "anonymous");
    }

    #[test]
    fn a_secured_client_is_counted_by_its_own_name() {
        let access = Access {
            actor: "practitioner-1".to_owned(),
            client: Some("app-1".to_owned()),
            scopes: Vec::new(),
            patient: None,
            secured: true,
        };
        assert_eq!(client_of(&access, &HeaderMap::new()), "app-1");
        let without_client = Access {
            client: None,
            ..access
        };
        assert_eq!(
            client_of(&without_client, &HeaderMap::new()),
            "practitioner-1"
        );
    }

    #[test]
    fn a_wait_of_any_length_is_at_least_a_second() {
        assert_eq!(waiting(1), 1);
        assert_eq!(waiting(999), 1);
        assert_eq!(waiting(1_000), 1);
        assert_eq!(waiting(1_001), 2);
    }
}
