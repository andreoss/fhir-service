use crate::Error;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{DecodingKey, Validation};
use serde_json::Value;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Es256,
    Es384,
    EdDsa,
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Ps384,
    Ps512,
}

impl Algorithm {
    pub fn named(name: &str) -> Option<Algorithm> {
        jsonwebtoken::Algorithm::from_str(name)
            .ok()
            .and_then(Algorithm::verifiable)
    }

    pub fn symmetric(name: &str) -> bool {
        jsonwebtoken::Algorithm::from_str(name)
            .is_ok_and(|named| named.family() == jsonwebtoken::AlgorithmFamily::Hmac)
    }

    fn verifiable(named: jsonwebtoken::Algorithm) -> Option<Algorithm> {
        match named {
            jsonwebtoken::Algorithm::ES256 => Some(Algorithm::Es256),
            jsonwebtoken::Algorithm::ES384 => Some(Algorithm::Es384),
            jsonwebtoken::Algorithm::EdDSA => Some(Algorithm::EdDsa),
            jsonwebtoken::Algorithm::RS256 => Some(Algorithm::Rs256),
            jsonwebtoken::Algorithm::RS384 => Some(Algorithm::Rs384),
            jsonwebtoken::Algorithm::RS512 => Some(Algorithm::Rs512),
            jsonwebtoken::Algorithm::PS256 => Some(Algorithm::Ps256),
            jsonwebtoken::Algorithm::PS384 => Some(Algorithm::Ps384),
            jsonwebtoken::Algorithm::PS512 => Some(Algorithm::Ps512),
            _ => None,
        }
    }

    fn checked(self) -> jsonwebtoken::Algorithm {
        match self {
            Algorithm::Es256 => jsonwebtoken::Algorithm::ES256,
            Algorithm::Es384 => jsonwebtoken::Algorithm::ES384,
            Algorithm::EdDsa => jsonwebtoken::Algorithm::EdDSA,
            Algorithm::Rs256 => jsonwebtoken::Algorithm::RS256,
            Algorithm::Rs384 => jsonwebtoken::Algorithm::RS384,
            Algorithm::Rs512 => jsonwebtoken::Algorithm::RS512,
            Algorithm::Ps256 => jsonwebtoken::Algorithm::PS256,
            Algorithm::Ps384 => jsonwebtoken::Algorithm::PS384,
            Algorithm::Ps512 => jsonwebtoken::Algorithm::PS512,
        }
    }

    fn of(entry: &Value) -> Option<Algorithm> {
        if let Some(name) = entry.get("alg").and_then(Value::as_str) {
            return Algorithm::named(name);
        }
        let kind = entry.get("kty").and_then(Value::as_str);
        let curve = entry.get("crv").and_then(Value::as_str);
        match (kind, curve) {
            (Some("EC"), Some("P-256")) => Some(Algorithm::Es256),
            (Some("EC"), Some("P-384")) => Some(Algorithm::Es384),
            (Some("OKP"), Some("Ed25519")) => Some(Algorithm::EdDsa),
            (Some("RSA"), _) => Some(Algorithm::Rs256),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub id: Option<String>,
    pub algorithm: Algorithm,
    published: Jwk,
}

impl Key {
    pub fn thumbprint(&self) -> Option<String> {
        let published = serde_json::to_value(&self.published).ok()?;
        let kind = published.get("kty").and_then(Value::as_str)?;
        let members: &[&str] = match kind {
            "RSA" => &["e", "kty", "n"],
            "EC" => &["crv", "kty", "x", "y"],
            "OKP" => &["crv", "kty", "x"],
            _ => return None,
        };
        let mut canonical = String::from("{");
        for (at, member) in members.iter().enumerate() {
            let value = published.get(*member).and_then(Value::as_str)?;
            if at > 0 {
                canonical.push(',');
            }
            canonical.push_str(&format!("{}:{}", enquoted(member), enquoted(value)));
        }
        canonical.push('}');
        let digest = <sha2::Sha256 as sha2::Digest>::digest(canonical.as_bytes());
        Some(base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            digest,
        ))
    }
}

fn enquoted(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
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
    
    
    
    pub roles: Vec<String>,
    pub patient: Option<String>,
    pub expires_at: Option<i64>,
    pub not_before: Option<i64>,
    pub issued_at: Option<i64>,
    
    
    
    
    pub carried: std::collections::BTreeMap<String, String>,
}

pub fn decode(text: &str) -> Result<Vec<u8>, Error> {
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| Error::Unauthenticated("malformed token".to_owned()))
}

pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}



fn scalars(payload: &Value) -> std::collections::BTreeMap<String, String> {
    let Some(held) = payload.as_object() else {
        return std::collections::BTreeMap::new();
    };
    held.iter()
        .filter_map(|(name, value)| match value {
            Value::String(held) => Some((name.clone(), held.clone())),
            Value::Number(held) => Some((name.clone(), held.to_string())),
            Value::Bool(held) => Some((name.clone(), held.to_string())),
            _ => None,
        })
        .collect()
}




fn roles(payload: &Value) -> Vec<String> {
    let mut held = text_or_texts(payload.get("roles"));
    held.extend(text_or_texts(payload.get("groups")));
    held.extend(text_or_texts(
        payload
            .get("realm_access")
            .and_then(|held| held.get("roles")),
    ));
    held.sort();
    held.dedup();
    held
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
            let shared = entry.get("kty").and_then(Value::as_str) == Some("oct")
                || entry
                    .get("alg")
                    .and_then(Value::as_str)
                    .is_some_and(Algorithm::symmetric);
            if shared {
                return Err(Error::Config("key set offers a symmetric key".to_owned()));
            }
            let Some(algorithm) = Algorithm::of(entry) else {
                continue;
            };
            let Ok(published) = serde_json::from_value::<Jwk>(entry.clone()) else {
                continue;
            };
            if DecodingKey::from_jwk(&published).is_err() {
                continue;
            }
            keys.push(Key {
                id: entry.get("kid").and_then(Value::as_str).map(str::to_owned),
                algorithm,
                published,
            });
        }
        match keys.is_empty() {
            true => Err(Error::Config("key set carries no usable key".to_owned())),
            false => Ok(KeySet { keys }),
        }
    }

    pub fn thumbprints(&self) -> Vec<String> {
        self.keys.iter().filter_map(Key::thumbprint).collect()
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
        Claims::verify_for(token, keys, issuer, None, now)
    }

    pub fn verify_for(
        token: &str,
        keys: &KeySet,
        issuer: &str,
        audience: Option<&str>,
        now: i64,
    ) -> Result<Claims, Error> {
        let refused = |reason: &str| Error::Unauthenticated(reason.to_owned());
        let header =
            jsonwebtoken::decode_header(token).map_err(|_| refused("malformed token header"))?;
        let algorithm = Algorithm::verifiable(header.alg)
            .ok_or_else(|| refused("unsupported signature algorithm"))?;
        let candidates = keys.matching(header.kid.as_deref(), algorithm);
        if candidates.is_empty() {
            return Err(refused("no key verifies this token"));
        }
        let checks = Claims::checks(algorithm);
        let payload = candidates
            .iter()
            .find_map(|key| {
                DecodingKey::from_jwk(&key.published)
                    .ok()
                    .and_then(|material| {
                        jsonwebtoken::decode::<Value>(token, &material, &checks).ok()
                    })
            })
            .ok_or_else(|| refused("signature does not verify"))?;
        let claims = Claims::read(&payload.claims);
        if claims.issuer != issuer {
            return Err(refused("token was issued elsewhere"));
        }
        if let Some(expected) = audience {
            if !claims.audience.iter().any(|named| named == expected) {
                return Err(refused("token names another audience"));
            }
        }
        if claims.expires_at.is_some_and(|at| now >= at) {
            return Err(refused("token has expired"));
        }
        if claims.not_before.is_some_and(|at| now < at) {
            return Err(refused("token is not yet valid"));
        }
        Ok(claims)
    }

    fn checks(algorithm: Algorithm) -> Validation {
        let mut checks = Validation::new(algorithm.checked());
        checks.required_spec_claims.clear();
        checks.validate_exp = false;
        checks.validate_nbf = false;
        checks.validate_aud = false;
        checks
    }

    fn read(payload: &Value) -> Claims {
        let text = |name: &str| payload.get(name).and_then(Value::as_str).map(str::to_owned);
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
            roles: roles(payload),
            patient: text("patient"),
            expires_at: count("exp"),
            not_before: count("nbf"),
            issued_at: count("iat"),
            carried: scalars(payload),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::fixture::Issuer;
    use serde_json::json;

    const ISSUER: &str = "https://issuer.example.org";

    fn payload() -> Value {
        json!({
            "iss": ISSUER,
            "sub": "practitioner-1",
            "client_id": "app-1",
            "aud": "https://service.example.org",
            "scope": "system/Patient.read system/Patient.write",
            "exp": 2_000,
            "nbf": 500,
            "iat": 500,
        })
    }

    fn header() -> Value {
        json!({"alg": "ES256", "typ": "JWT", "kid": "one"})
    }

    #[test]
    fn the_encoding_round_trips_without_padding() {
        assert_eq!(
            encode(b"any carnal pleasure."),
            "YW55IGNhcm5hbCBwbGVhc3VyZS4"
        );
        assert_eq!(
            decode("YW55IGNhcm5hbCBwbGVhc3VyZS4").unwrap(),
            b"any carnal pleasure."
        );
        assert_eq!(
            decode(&encode(&[0xff, 0xfe, 0x00, 0x01])).unwrap(),
            vec![0xff, 0xfe, 0x00, 0x01]
        );
        assert!(decode("not base64 ~").is_err());
        assert!(decode("A").is_err());
        assert!(decode("YW55IGNhcm5hbCBwbGVhc3VyZS4=").is_err());
    }

    const AUDIENCE: &str = "https://service.example.org";

    #[test]
    fn a_token_minted_for_another_audience_is_refused() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let error = Claims::verify_for(
            &issuer.mint(&payload()),
            &keys,
            ISSUER,
            Some("https://unrelated-service.example.org"),
            1_000,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("audience"), "{error:?}");
    }

    #[test]
    fn a_token_minted_for_this_audience_is_accepted() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let claims = Claims::verify_for(
            &issuer.mint(&payload()),
            &keys,
            ISSUER,
            Some(AUDIENCE),
            1_000,
        )
        .unwrap();
        assert_eq!(claims.audience, vec![AUDIENCE.to_owned()]);
    }

    #[test]
    fn one_audience_among_several_is_enough() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let mut body = payload();
        body["aud"] = json!(["https://elsewhere.example.org", AUDIENCE]);
        let claims =
            Claims::verify_for(&issuer.mint(&body), &keys, ISSUER, Some(AUDIENCE), 1_000).unwrap();
        assert_eq!(claims.audience.len(), 2);
    }

    #[test]
    fn a_token_carrying_no_audience_is_refused_when_one_is_expected() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let mut body = payload();
        body.as_object_mut().unwrap().remove("aud");
        let error = Claims::verify_for(&issuer.mint(&body), &keys, ISSUER, Some(AUDIENCE), 1_000)
            .unwrap_err();
        assert!(format!("{error:?}").contains("audience"), "{error:?}");
    }

    #[test]
    fn a_signed_token_yields_the_claims_it_carries() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let claims = Claims::verify(&issuer.mint(&payload()), &keys, ISSUER, 1_000).unwrap();
        assert_eq!(claims.issuer, ISSUER);
        assert_eq!(claims.subject.as_deref(), Some("practitioner-1"));
        assert_eq!(claims.client.as_deref(), Some("app-1"));
        assert_eq!(
            claims.audience,
            vec!["https://service.example.org".to_owned()]
        );
        assert_eq!(
            claims.scopes,
            vec![
                "system/Patient.read".to_owned(),
                "system/Patient.write".to_owned()
            ]
        );
        assert_eq!(claims.expires_at, Some(2_000));
    }

    #[test]
    fn a_published_key_set_names_the_algorithm_it_verifies() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        assert_eq!(keys.keys.len(), 1);
        assert_eq!(keys.keys[0].id.as_deref(), Some("one"));
        assert_eq!(keys.keys[0].algorithm, Algorithm::Es256);
    }

    #[test]
    fn a_token_signed_by_another_key_is_refused() {
        let issuer = Issuer::generate("one");
        let other = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let error = Claims::verify(&other.mint(&payload()), &keys, ISSUER, 1_000).unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
    }

    #[test]
    fn a_token_carrying_no_signature_is_refused() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let input = Issuer::input(&header(), &payload());
        assert!(matches!(
            Claims::verify(&format!("{input}."), &keys, ISSUER, 1_000).unwrap_err(),
            Error::Unauthenticated(_)
        ));
        let unsigned = Issuer::input(&json!({"alg": "none"}), &payload());
        assert!(matches!(
            Claims::verify(&format!("{unsigned}."), &keys, ISSUER, 1_000).unwrap_err(),
            Error::Unauthenticated(_)
        ));
    }

    #[test]
    fn a_token_naming_a_symmetric_algorithm_is_refused() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let head = json!({"alg": "HS256", "kid": "one"});
        let input = Issuer::input(&head, &payload());
        let forged = jsonwebtoken::crypto::sign(
            input.as_bytes(),
            &jsonwebtoken::EncodingKey::from_secret(issuer.material().as_bytes()),
            jsonwebtoken::Algorithm::HS256,
        )
        .expect("the library signs");
        let error = Claims::verify(&format!("{input}.{forged}"), &keys, ISSUER, 1_000).unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
        assert!(error.to_string().contains("unsupported"));
    }

    #[test]
    fn a_token_naming_an_algorithm_the_key_does_not_verify_is_refused() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let token = issuer.minted_under(&json!({"alg": "ES384", "kid": "one"}), &payload());
        let error = Claims::verify(&token, &keys, ISSUER, 1_000).unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
    }

    #[test]
    fn a_key_set_offering_a_symmetric_key_is_refused() {
        let shared = json!({"keys": [
            {"kty": "oct", "kid": "one", "alg": "HS256", "k": encode(b"a-shared-secret")}
        ]});
        assert!(matches!(KeySet::parse(&shared), Err(Error::Config(_))));
        let named = json!({"keys": [{"kty": "EC", "crv": "P-256", "alg": "HS256"}]});
        assert!(matches!(KeySet::parse(&named), Err(Error::Config(_))));
    }

    #[test]
    fn an_expired_or_early_token_is_refused() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let token = issuer.mint(&payload());
        assert!(matches!(
            Claims::verify(&token, &keys, ISSUER, 2_001).unwrap_err(),
            Error::Unauthenticated(_)
        ));
        assert!(matches!(
            Claims::verify(&token, &keys, ISSUER, 499).unwrap_err(),
            Error::Unauthenticated(_)
        ));
    }

    #[test]
    fn a_token_from_another_issuer_is_refused() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let token = issuer.mint(&payload());
        assert!(matches!(
            Claims::verify(&token, &keys, "https://elsewhere.example.org", 1_000).unwrap_err(),
            Error::Unauthenticated(_)
        ));
    }

    #[test]
    fn a_malformed_token_is_refused_without_echoing_it() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let error = Claims::verify("not-a-token", &keys, ISSUER, 1_000).unwrap_err();
        assert!(matches!(error, Error::Unauthenticated(_)));
        assert!(!error.to_string().contains("not-a-token"));
    }

    #[test]
    fn a_named_key_is_the_one_the_header_asks_for() {
        let stale = Issuer::generate("one");
        let current = Issuer::generate("two");
        let mut document = stale.keys();
        let listed = document["keys"].as_array_mut().expect("a listed key");
        listed.push(current.keys()["keys"][0].clone());
        let keys = KeySet::parse(&document).unwrap();
        assert_eq!(keys.keys.len(), 2);
        assert!(Claims::verify(&current.mint(&payload()), &keys, ISSUER, 1_000).is_ok());
    }

    #[test]
    fn a_launch_compartment_and_a_list_of_audiences_are_carried() {
        let issuer = Issuer::generate("one");
        let keys = KeySet::parse(&issuer.keys()).unwrap();
        let mut body = payload();
        body["patient"] = json!("pt-1");
        body["aud"] = json!(["one", "two"]);
        let claims = Claims::verify(&issuer.mint(&body), &keys, ISSUER, 1_000).unwrap();
        assert_eq!(claims.patient.as_deref(), Some("pt-1"));
        assert_eq!(claims.audience, vec!["one".to_owned(), "two".to_owned()]);
    }

    #[test]
    fn a_key_set_without_usable_keys_is_refused() {
        assert!(KeySet::parse(&json!({"keys": []})).is_err());
        assert!(KeySet::parse(&json!({"keys": [{"kty": "OKP", "kid": "r"}]})).is_err());
        assert!(KeySet::parse(&json!({})).is_err());
    }
}
