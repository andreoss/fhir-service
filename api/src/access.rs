use axum::http::header::{self, HeaderMap};
use fhir_core::security::bearer::Claims;
use fhir_core::security::Access;
use fhir_core::Error;
use std::sync::Arc;

use crate::app::AppState;
use crate::discovery::Keys;
use crate::smart::Authorization;

pub struct Guard {
    authorization: Arc<Authorization>,
    keys: Arc<dyn Keys>,
}

impl Guard {
    pub fn new(authorization: Arc<Authorization>, keys: Arc<dyn Keys>) -> Guard {
        Guard { authorization, keys }
    }

    pub fn issuer(&self) -> &str {
        &self.authorization.issuer
    }

    pub async fn claims(&self, token: &str) -> Result<Claims, Error> {
        let keys = self.keys.keys(&self.authorization.issuer).await?;
        Claims::verify(token, &keys, &self.authorization.issuer, now())
    }

    pub async fn access(&self, headers: &HeaderMap) -> Result<Access, Error> {
        let carried = headers
            .get(header::AUTHORIZATION)
            .ok_or_else(|| Error::Unauthenticated("a bearer token is required".to_owned()))?
            .to_str()
            .map_err(|_| Error::Unauthenticated("the credential is not ascii".to_owned()))?;
        let token = bearer(carried)
            .ok_or_else(|| Error::Unauthenticated("a bearer token is required".to_owned()))?;
        Ok(Access::of(&self.claims(token).await?))
    }
}

fn bearer(carried: &str) -> Option<&str> {
    let (scheme, token) = carried.split_once(' ')?;
    match scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
        true => Some(token.trim()),
        false => None,
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

pub async fn access_of(state: &AppState, headers: &HeaderMap) -> Result<Access, Error> {
    match &state.guard {
        None => Ok(Access::open()),
        Some(guard) => guard.access(headers).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::HeldKeys;
    use fhir_core::security::bearer::{encode, KeySet};
    use fhir_core::security::scope::DataAction;
    use fhir_core::security::digest::hmac_sha256;
    use serde_json::json;

    const SECRET: &[u8] = b"a-secret-from-the-store";

    fn guard() -> Guard {
        let set = KeySet::parse(&json!({"keys": [
            {"kty": "oct", "kid": "one", "alg": "HS256", "k": encode(SECRET)}
        ]}))
        .expect("a configured key set");
        Guard::new(
            Arc::new(Authorization::new(
                "https://issuer.example.org",
                "https://issuer.example.org/a",
                "https://issuer.example.org/t",
            )),
            Arc::new(HeldKeys::new(set)),
        )
    }

    fn token(scopes: &str) -> String {
        let head = encode(&serde_json::to_vec(&json!({"alg": "HS256", "kid": "one"})).unwrap());
        let body = encode(
            &serde_json::to_vec(&json!({
                "iss": "https://issuer.example.org",
                "sub": "practitioner-1",
                "scope": scopes,
                "exp": now() + 300,
            }))
            .unwrap(),
        );
        let input = format!("{head}.{body}");
        format!("{input}.{}", encode(&hmac_sha256(SECRET, input.as_bytes())))
    }

    fn carrying(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, value.parse().expect("a header value"));
        headers
    }

    #[tokio::test]
    async fn a_request_without_a_token_is_not_authenticated() {
        let error = guard().access(&HeaderMap::new()).await.unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
        assert_eq!(error.http_status(), 401);
    }

    #[tokio::test]
    async fn a_credential_of_another_scheme_is_refused() {
        let error = guard().access(&carrying("Basic abc")).await.unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
    }

    #[tokio::test]
    async fn a_valid_token_yields_the_access_it_grants() {
        let headers = carrying(&format!("Bearer {}", token("system/Patient.read")));
        let access = guard().access(&headers).await.expect("the token is valid");
        assert_eq!(access.actor, "practitioner-1");
        assert!(access.permits(DataAction::Read, Some("Patient".parse().unwrap())));
        assert!(!access.permits(DataAction::Write, Some("Patient".parse().unwrap())));
    }

    #[tokio::test]
    async fn the_scheme_is_read_without_regard_to_case() {
        let headers = carrying(&format!("bearer {}", token("system/Patient.read")));
        assert!(guard().access(&headers).await.is_ok());
    }

    #[tokio::test]
    async fn a_forged_token_is_refused_without_reaching_the_claims() {
        let forged = format!("Bearer {}x", token("system/*.read"));
        let error = guard().access(&carrying(&forged)).await.unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
        assert!(!error.to_string().contains(&forged));
    }
}
