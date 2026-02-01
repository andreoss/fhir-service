use crate::Error;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::DecodingKey;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Hs256,
}

impl Algorithm {
    pub fn named(name: &str) -> Option<Algorithm> {
        match name {
            "HS256" => Some(Algorithm::Hs256),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub id: Option<String>,
    pub algorithm: Algorithm,
    pub secret: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeySet {
    pub keys: Vec<Key>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claims {
    pub issuer: String,
    pub subject: Option<String>,
    pub client: Option<String>,
    pub audience: Vec<String>,
    pub scopes: Vec<String>,
    pub patient: Option<String>,
    pub expires_at: Option<i64>,
    pub not_before: Option<i64>,
    pub issued_at: Option<i64>,
}

pub fn decode(text: &str) -> Result<Vec<u8>, Error> {
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| Error::Unauthenticated("malformed token".to_owned()))
}

pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn text_or_texts(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(single)) => vec![single.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

impl KeySet {
    pub fn parse(document: &Value) -> Result<KeySet, Error> {
        let listed = document
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Config("key set carries no keys".to_owned()))?;
        let mut keys = Vec::new();
        for entry in listed {
            let algorithm = entry
                .get("alg")
                .and_then(Value::as_str)
                .and_then(Algorithm::named);
            let material = entry.get("k").and_then(Value::as_str);
            let (Some(algorithm), Some(material)) = (algorithm, material) else {
                continue;
            };
            keys.push(Key {
                id: entry.get("kid").and_then(Value::as_str).map(str::to_owned),
                algorithm,
                secret: decode(material)?,
            });
        }
        match keys.is_empty() {
            true => Err(Error::Config("key set carries no usable key".to_owned())),
            false => Ok(KeySet { keys }),
        }
    }

    fn matching(&self, id: Option<&str>, algorithm: Algorithm) -> Vec<&Key> {
        self.keys
            .iter()
            .filter(|key| key.algorithm == algorithm)
            .filter(|key| match (id, key.id.as_deref()) {
                (Some(asked), Some(held)) => asked == held,
                _ => true,
            })
            .collect()
    }
}

impl Claims {
    pub fn verify(token: &str, keys: &KeySet, issuer: &str, now: i64) -> Result<Claims, Error> {
        let refused = |reason: &str| Error::Unauthenticated(reason.to_owned());
        let mut parts = token.split('.');
        let (Some(head), Some(body), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(refused("malformed token"));
        };
        let header: Value = serde_json::from_slice(&decode(head)?)
            .map_err(|_| refused("malformed token header"))?;
        let algorithm = header
            .get("alg")
            .and_then(Value::as_str)
            .and_then(Algorithm::named)
            .ok_or_else(|| refused("unsupported signature algorithm"))?;
        let named = header.get("kid").and_then(Value::as_str);
        let candidates = keys.matching(named, algorithm);
        if candidates.is_empty() {
            return Err(refused("no key verifies this token"));
        }
        let input = format!("{head}.{body}");
        let verified = candidates.iter().any(|key| match key.algorithm {
            Algorithm::Hs256 => jsonwebtoken::crypto::verify(
                signature,
                input.as_bytes(),
                &DecodingKey::from_secret(&key.secret),
                jsonwebtoken::Algorithm::HS256,
            )
            .unwrap_or(false),
        });
        if !verified {
            return Err(refused("signature does not verify"));
        }
        let payload: Value =
            serde_json::from_slice(&decode(body)?).map_err(|_| refused("malformed token body"))?;
        let claims = Claims::read(&payload);
        if claims.issuer != issuer {
            return Err(refused("token was issued elsewhere"));
        }
        if claims.expires_at.is_some_and(|at| now >= at) {
            return Err(refused("token has expired"));
        }
        if claims.not_before.is_some_and(|at| now < at) {
            return Err(refused("token is not yet valid"));
        }
        Ok(claims)
    }

    fn read(payload: &Value) -> Claims {
        let text = |name: &str| {
            payload
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let count = |name: &str| payload.get(name).and_then(Value::as_i64);
        Claims {
            issuer: text("iss").unwrap_or_default(),
            subject: text("sub"),
            client: text("client_id").or_else(|| text("azp")),
            audience: text_or_texts(payload.get("aud")),
            scopes: text("scope")
                .map(|listed| {
                    listed
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_else(|| text_or_texts(payload.get("scp"))),
            patient: text("patient"),
            expires_at: count("exp"),
            not_before: count("nbf"),
            issued_at: count("iat"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signed(payload: Value, secret: &[u8], header: Value) -> String {
        let head = encode(&serde_json::to_vec(&header).unwrap());
        let body = encode(&serde_json::to_vec(&payload).unwrap());
        let input = format!("{head}.{body}");
        let mac = jsonwebtoken::crypto::sign(
            input.as_bytes(),
            &jsonwebtoken::EncodingKey::from_secret(secret),
            jsonwebtoken::Algorithm::HS256,
        )
        .expect("the library signs");
        format!("{input}.{mac}")
    }

    fn header() -> Value {
        json!({"alg": "HS256", "typ": "JWT", "kid": "one"})
    }

    fn key_set(secret: &[u8]) -> KeySet {
        KeySet::parse(&json!({"keys": [
            {"kty": "oct", "kid": "one", "alg": "HS256", "k": encode(secret)}
        ]}))
        .expect("a published key set parses")
    }

    fn payload() -> Value {
        json!({
            "iss": "https://issuer.example.org",
            "sub": "practitioner-1",
            "client_id": "app-1",
            "aud": "https://service.example.org",
            "scope": "system/Patient.read system/Patient.write",
            "exp": 2_000,
            "nbf": 500,
            "iat": 500,
        })
    }

    #[test]
    fn the_encoding_round_trips_without_padding() {
        assert_eq!(encode(b"any carnal pleasure."), "YW55IGNhcm5hbCBwbGVhc3VyZS4");
        assert_eq!(decode("YW55IGNhcm5hbCBwbGVhc3VyZS4").unwrap(), b"any carnal pleasure.");
        assert_eq!(decode(&encode(&[0xff, 0xfe, 0x00, 0x01])).unwrap(), vec![0xff, 0xfe, 0x00, 0x01]);
        assert!(decode("not base64 ~").is_err());
        assert!(decode("A").is_err());
        assert!(decode("YW55IGNhcm5hbCBwbGVhc3VyZS4=").is_err());
    }

    #[test]
    fn a_signed_token_yields_the_claims_it_carries() {
        let secret = b"a-configured-secret";
        let claims = Claims::verify(&signed(payload(), secret, header()), &key_set(secret), "https://issuer.example.org", 1_000).unwrap();
        assert_eq!(claims.issuer, "https://issuer.example.org");
        assert_eq!(claims.subject.as_deref(), Some("practitioner-1"));
        assert_eq!(claims.client.as_deref(), Some("app-1"));
        assert_eq!(claims.audience, vec!["https://service.example.org".to_owned()]);
        assert_eq!(claims.scopes, vec!["system/Patient.read".to_owned(), "system/Patient.write".to_owned()]);
        assert_eq!(claims.expires_at, Some(2_000));
    }

    #[test]
    fn a_token_signed_by_another_key_is_refused() {
        let token = signed(payload(), b"another-secret", header());
        let error = Claims::verify(&token, &key_set(b"a-configured-secret"), "https://issuer.example.org", 1_000).unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
    }

    #[test]
    fn an_expired_or_early_token_is_refused() {
        let secret = b"a-configured-secret";
        let token = signed(payload(), secret, header());
        assert!(matches!(
            Claims::verify(&token, &key_set(secret), "https://issuer.example.org", 2_001).unwrap_err(),
            Error::Unauthenticated(_)
        ));
        assert!(matches!(
            Claims::verify(&token, &key_set(secret), "https://issuer.example.org", 499).unwrap_err(),
            Error::Unauthenticated(_)
        ));
    }

    #[test]
    fn a_token_from_another_issuer_is_refused() {
        let secret = b"a-configured-secret";
        let token = signed(payload(), secret, header());
        assert!(matches!(
            Claims::verify(&token, &key_set(secret), "https://elsewhere.example.org", 1_000).unwrap_err(),
            Error::Unauthenticated(_)
        ));
    }

    #[test]
    fn an_unsigned_token_is_refused() {
        let secret = b"a-configured-secret";
        let head = encode(&serde_json::to_vec(&json!({"alg": "none"})).unwrap());
        let body = encode(&serde_json::to_vec(&payload()).unwrap());
        let error = Claims::verify(&format!("{head}.{body}."), &key_set(secret), "https://issuer.example.org", 1_000).unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
    }

    #[test]
    fn a_malformed_token_is_refused_without_echoing_it() {
        let secret = b"a-configured-secret";
        let error = Claims::verify("not-a-token", &key_set(secret), "https://issuer.example.org", 1_000).unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
        assert!(!error.to_string().contains("not-a-token"));
    }

    #[test]
    fn a_named_key_is_the_one_the_header_asks_for() {
        let secret = b"a-configured-secret";
        let mut keys = key_set(b"stale-secret");
        keys.keys.push(Key {
            id: Some("two".to_owned()),
            algorithm: Algorithm::Hs256,
            secret: secret.to_vec(),
        });
        let token = signed(payload(), secret, json!({"alg": "HS256", "kid": "two"}));
        assert!(Claims::verify(&token, &keys, "https://issuer.example.org", 1_000).is_ok());
    }

    #[test]
    fn a_launch_compartment_and_a_list_of_audiences_are_carried() {
        let secret = b"a-configured-secret";
        let mut body = payload();
        body["patient"] = json!("pt-1");
        body["aud"] = json!(["one", "two"]);
        let claims = Claims::verify(&signed(body, secret, header()), &key_set(secret), "https://issuer.example.org", 1_000).unwrap();
        assert_eq!(claims.patient.as_deref(), Some("pt-1"));
        assert_eq!(claims.audience, vec!["one".to_owned(), "two".to_owned()]);
    }

    #[test]
    fn a_key_set_without_usable_keys_is_refused() {
        assert!(KeySet::parse(&json!({"keys": []})).is_err());
        assert!(KeySet::parse(&json!({"keys": [{"kty": "RSA", "kid": "r"}]})).is_err());
        assert!(KeySet::parse(&json!({})).is_err());
    }
}
