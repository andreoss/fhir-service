use axum::http::header::{self, HeaderMap};
use fhir_core::security::bearer::Claims;
use fhir_core::search::{Compartment, Filter, Grant, GrantFilter, Registry};
use fhir_core::security::scope::{DataAction, Scope, Subject};
use fhir_core::security::Access;
use fhir_core::ResourceType;
use fhir_core::Error;
use std::sync::Arc;

use crate::app::AppState;
use crate::discovery::Keys;
use crate::smart::Authorization;

const PATIENT_COMPARTMENT: &str = "Patient";

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

pub fn granted(
    registry: &Registry,
    access: &Access,
    action: DataAction,
) -> Result<Option<Grant>, Error> {
    if !access.secured {
        return Ok(None);
    }
    let carrying: Vec<&Scope> = access
        .scopes
        .iter()
        .filter(|scope| scope.actions.contains(&action))
        .collect();
    if carrying.is_empty() {
        return Err(Error::Forbidden(action.as_str().to_owned()));
    }
    let mut types = Vec::new();
    for scope in &carrying {
        match scope.target {
            None => {
                types.clear();
                break;
            }
            Some(kind) if !types.contains(&kind) => types.push(kind),
            Some(_) => {}
        }
    }
    let confined = carrying
        .iter()
        .all(|scope| scope.subject == Subject::Patient);
    let compartments = match (confined, &access.patient) {
        (true, Some(id)) => vec![Compartment {
            kind: PATIENT_COMPARTMENT.parse::<ResourceType>()?,
            id: id.clone(),
        }],
        _ => Vec::new(),
    };
    let mut filters = Vec::new();
    let mut unfiltered = Vec::new();
    for scope in &carrying {
        match (scope.target, scope.filters.is_empty()) {
            (_, true) => unfiltered.push(scope.target),
            (None, false) => {}
            (Some(kind), false) => {
                for filter in narrowed(registry, kind, &scope.filters)? {
                    filters.push(GrantFilter {
                        resource_type: kind,
                        filter,
                    });
                }
            }
        }
    }
    if unfiltered.contains(&None) {
        filters.clear();
    } else {
        filters.retain(|held| !unfiltered.contains(&Some(held.resource_type)));
    }
    Ok(Some(Grant {
        types,
        compartments,
        filters,
    }))
}

fn narrowed(
    registry: &Registry,
    resource_type: ResourceType,
    pairs: &[(String, String)],
) -> Result<Vec<Filter>, Error> {
    let raw = pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<String>>()
        .join("&");
    let query = crate::search::parse_query(registry, Some(resource_type), Some(&raw))?;
    match query.chains.is_empty() && query.includes.is_empty() {
        true => Ok(query.filters),
        false => Err(Error::InvalidParameter(
            "a scope narrows by its own parameters only".to_owned(),
        )),
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

    fn registry() -> Registry {
        Registry::for_version(fhir_core::FhirVersion::R4)
    }

    fn access_with(scopes: &[&str], patient: Option<&str>) -> Access {
        Access {
            actor: "practitioner-1".to_owned(),
            client: None,
            scopes: Scope::parse_all(
                &scopes.iter().map(|entry| (*entry).to_owned()).collect::<Vec<String>>(),
            ),
            patient: patient.and_then(|id| fhir_core::ResourceId::parse(id).ok()),
            secured: true,
        }
    }

    #[test]
    fn an_unsecured_access_confines_nothing() {
        let grant = granted(&registry(), &Access::open(), DataAction::Read).unwrap();
        assert!(grant.is_none());
    }

    #[test]
    fn a_typed_scope_confines_the_grant_to_its_types() {
        let access = access_with(&["system/Patient.read", "system/Observation.read"], None);
        let grant = granted(&registry(), &access, DataAction::Read).unwrap().unwrap();
        assert_eq!(grant.types.len(), 2);
        assert!(grant.admits("Patient".parse().unwrap()));
        assert!(!grant.admits("Encounter".parse().unwrap()));
        assert!(grant.is_open());
    }

    #[test]
    fn a_wildcard_scope_leaves_the_types_open() {
        let access = access_with(&["system/*.read", "system/Patient.read"], None);
        let grant = granted(&registry(), &access, DataAction::Read).unwrap().unwrap();
        assert!(grant.types.is_empty());
        assert!(grant.admits("Encounter".parse().unwrap()));
    }

    #[test]
    fn a_launch_compartment_confines_a_patient_scope() {
        let access = access_with(&["patient/Observation.rs"], Some("pt-1"));
        let grant = granted(&registry(), &access, DataAction::Read).unwrap().unwrap();
        assert!(!grant.is_open());
        assert_eq!(grant.compartments[0].id.as_str(), "pt-1");
    }

    #[test]
    fn a_user_scope_alongside_a_patient_scope_lifts_the_compartment() {
        let access = access_with(&["patient/Observation.rs", "user/Observation.rs"], Some("pt-1"));
        let grant = granted(&registry(), &access, DataAction::Read).unwrap().unwrap();
        assert!(grant.is_open());
    }

    #[test]
    fn a_scope_parameter_narrows_only_its_own_type() {
        let access = access_with(&["system/Observation.rs?status=final"], None);
        let grant = granted(&registry(), &access, DataAction::Read).unwrap().unwrap();
        assert_eq!(grant.narrowing("Observation".parse().unwrap()).len(), 1);
        assert!(grant.narrowing("Patient".parse().unwrap()).is_empty());
    }

    #[test]
    fn a_broader_scope_over_the_same_type_drops_the_narrowing() {
        let access = access_with(
            &["system/Observation.rs?status=final", "system/Observation.read"],
            None,
        );
        let grant = granted(&registry(), &access, DataAction::Read).unwrap().unwrap();
        assert!(grant.filters.is_empty());
    }

    #[test]
    fn an_action_no_scope_carries_yields_no_grant_at_all() {
        let access = access_with(&["system/Patient.read"], None);
        assert!(granted(&registry(), &access, DataAction::Write).is_err());
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
