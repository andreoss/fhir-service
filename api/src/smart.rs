use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use fhir_core::Error;
use serde_json::{json, Map, Value};

use crate::app::AppState;
use crate::handlers::AppError;

const JSON: &str = "application/json";

const OAUTH_URIS: &str = "http://fhir-registry.smarthealthit.org/StructureDefinition/oauth-uris";
const SECURITY_SERVICE: &str = "http://terminology.hl7.org/CodeSystem/restful-security-service";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    pub issuer: String,
    pub audience: Option<String>,
    pub jwks: Option<String>,
    pub authorize: String,
    pub token: String,
    pub introspect: Option<String>,
    pub scopes: Vec<String>,
    pub capabilities: Vec<String>,
}

impl Authorization {
    pub fn new(issuer: &str, authorize: &str, token: &str) -> Authorization {
        Authorization {
            issuer: issuer.to_owned(),
            audience: None,
            jwks: None,
            authorize: authorize.to_owned(),
            token: token.to_owned(),
            introspect: None,
            scopes: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    pub fn with_audience(self, audience: &str) -> Authorization {
        Authorization {
            audience: Some(audience.to_owned()),
            ..self
        }
    }

    pub fn with_jwks(self, jwks: &str) -> Authorization {
        Authorization {
            jwks: Some(jwks.to_owned()),
            ..self
        }
    }

    pub fn with_introspection(self, endpoint: &str) -> Authorization {
        Authorization {
            introspect: Some(endpoint.to_owned()),
            ..self
        }
    }

    pub fn with_scopes(self, scopes: Vec<String>) -> Authorization {
        Authorization { scopes, ..self }
    }

    pub fn with_capabilities(self, capabilities: Vec<String>) -> Authorization {
        Authorization {
            capabilities,
            ..self
        }
    }

    pub fn document(&self) -> Value {
        let mut found = Map::new();
        found.insert("issuer".to_owned(), json!(self.issuer));
        found.insert("authorization_endpoint".to_owned(), json!(self.authorize));
        found.insert("token_endpoint".to_owned(), json!(self.token));
        if let Some(endpoint) = &self.introspect {
            found.insert("introspection_endpoint".to_owned(), json!(endpoint));
        }
        if let Some(address) = &self.jwks {
            found.insert("jwks_uri".to_owned(), json!(address));
        }
        found.insert("scopes_supported".to_owned(), json!(self.scopes));
        found.insert("capabilities".to_owned(), json!(self.capabilities));
        found.insert("response_types_supported".to_owned(), json!(["code"]));
        found.insert(
            "grant_types_supported".to_owned(),
            json!(["authorization_code", "client_credentials"]),
        );
        found.insert(
            "code_challenge_methods_supported".to_owned(),
            json!(["S256"]),
        );
        Value::Object(found)
    }
}

pub fn security(state: &AppState) -> Option<Value> {
    let active = state.authorization.as_ref()?;
    let mut uris = vec![
        json!({"url": "authorize", "valueUri": active.authorize}),
        json!({"url": "token", "valueUri": active.token}),
    ];
    if let Some(endpoint) = &active.introspect {
        uris.push(json!({"url": "introspect", "valueUri": endpoint}));
    }
    Some(json!({
        "cors": true,
        "service": [{
            "coding": [{"system": SECURITY_SERVICE, "code": "SMART-on-FHIR"}],
        }],
        "extension": [{"url": OAUTH_URIS, "extension": uris}],
    }))
}

pub async fn configuration(State(state): State<AppState>) -> Result<Response, AppError> {
    let active = state.authorization.as_ref().ok_or(Error::NotFound)?;
    let body = serde_json::to_vec(&active.document())
        .map_err(|error| Error::Internal(error.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_carries_the_endpoints_it_was_built_from() {
        let active = Authorization::new(
            "https://issuer.example.org",
            "https://issuer.example.org/authorize",
            "https://issuer.example.org/token",
        )
        .with_scopes(vec!["system/*.read".to_owned()]);
        let document = active.document();
        assert_eq!(document["issuer"], "https://issuer.example.org");
        assert_eq!(
            document["authorization_endpoint"],
            "https://issuer.example.org/authorize"
        );
        assert_eq!(
            document["token_endpoint"],
            "https://issuer.example.org/token"
        );
        assert_eq!(document["scopes_supported"][0], "system/*.read");
        assert!(document.get("introspection_endpoint").is_none());
    }

    #[test]
    fn introspection_is_published_when_it_is_offered() {
        let active = Authorization::new("i", "a", "t").with_introspection("x");
        assert_eq!(active.document()["introspection_endpoint"], "x");
    }

    #[test]
    fn the_key_set_address_is_published_when_it_is_held() {
        let active =
            Authorization::new("i", "a", "t").with_jwks("https://issuer.example.org/certs");
        assert_eq!(
            active.document()["jwks_uri"],
            "https://issuer.example.org/certs"
        );
    }

    #[test]
    fn a_document_without_a_key_set_address_carries_no_field_for_it() {
        assert!(Authorization::new("i", "a", "t")
            .document()
            .get("jwks_uri")
            .is_none());
    }
}
