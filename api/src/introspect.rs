use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{self, HeaderMap};
use axum::response::{IntoResponse, Response};
use fhir_core::security::bearer::Claims;
use fhir_core::Error;
use serde_json::{json, Map, Value};

use crate::app::AppState;
use crate::handlers::AppError;

const JSON: &str = "application/json";

pub const INTROSPECT: &str = "/_introspect";

pub async fn introspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let guard = state.guard.as_ref().ok_or(Error::NotFound)?;
    guard.access(&headers).await?;
    let asked = presented(&body).ok_or_else(|| {
        Error::InvalidParameter("a token is introspected by presenting one".to_owned())
    })?;
    let reported = match guard.claims(&asked).await {
        Err(_) => json!({"active": false}),
        Ok(claims) => reply(&claims),
    };
    let rendered =
        serde_json::to_vec(&reported).map_err(|error| Error::Internal(error.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, JSON),
            (header::CACHE_CONTROL, "no-store"),
        ],
        rendered,
    )
        .into_response())
}

fn reply(claims: &Claims) -> Value {
    let mut found = Map::new();
    found.insert("active".to_owned(), json!(true));
    found.insert("iss".to_owned(), json!(claims.issuer));
    found.insert("scope".to_owned(), json!(claims.scopes.join(" ")));
    for (name, held) in [
        ("sub", &claims.subject),
        ("client_id", &claims.client),
        ("patient", &claims.patient),
    ] {
        if let Some(value) = held {
            found.insert(name.to_owned(), json!(value));
        }
    }
    for (name, held) in [
        ("exp", claims.expires_at),
        ("iat", claims.issued_at),
        ("nbf", claims.not_before),
    ] {
        if let Some(value) = held {
            found.insert(name.to_owned(), json!(value));
        }
    }
    if !claims.audience.is_empty() {
        found.insert("aud".to_owned(), json!(claims.audience));
    }
    Value::Object(found)
}

fn presented(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(Value::Object(carried)) = serde_json::from_str::<Value>(text) {
        return carried
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    text.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "token")
        .map(|(_, value)| decoded(value))
}

fn decoded(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(' '),
            b'%' => {
                let high = bytes.next().unwrap_or(b'0');
                let low = bytes.next().unwrap_or(b'0');
                let pair = format!("{}{}", high as char, low as char);
                match u8::from_str_radix(&pair, 16) {
                    Ok(value) => out.push(value as char),
                    Err(_) => out.push('%'),
                }
            }
            other => out.push(other as char),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_read_from_a_form_or_a_document() {
        assert_eq!(presented(b"token=abc").as_deref(), Some("abc"));
        assert_eq!(
            presented(b"token_type_hint=access_token&token=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(presented(br#"{"token":"abc"}"#).as_deref(), Some("abc"));
        assert_eq!(presented(b"token=a%2Eb").as_deref(), Some("a.b"));
        assert!(presented(b"").is_none());
        assert!(presented(b"hint=none").is_none());
    }

    #[test]
    fn a_reply_carries_the_claims_and_nothing_more() {
        let claims = Claims {
            issuer: "https://issuer.example.org".to_owned(),
            subject: Some("practitioner-1".to_owned()),
            scopes: vec!["system/Patient.read".to_owned()],
            expires_at: Some(2_000),
            ..Claims::default()
        };
        let reported = reply(&claims);
        assert_eq!(reported["active"], true);
        assert_eq!(reported["scope"], "system/Patient.read");
        assert_eq!(reported["sub"], "practitioner-1");
        assert_eq!(reported["exp"], 2_000);
        assert!(reported.get("client_id").is_none());
        assert!(reported.get("aud").is_none());
    }
}
