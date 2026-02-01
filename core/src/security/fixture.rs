use crate::security::bearer::encode;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use p256::ecdsa::SigningKey;
use p256::pkcs8::EncodePrivateKey;
use serde_json::{json, Value};

pub struct Issuer {
    id: String,
    signing: EncodingKey,
    published: Value,
}

impl Issuer {
    pub fn generate(id: &str) -> Issuer {
        let signing = SigningKey::random(&mut rand_core::OsRng);
        let der = signing.to_pkcs8_der().expect("a generated key encodes");
        let signing = EncodingKey::from_ec_der(der.as_bytes());
        let key = Jwk::from_encoding_key(&signing, Algorithm::ES256).expect("a key publishes");
        let mut published = serde_json::to_value(&key).expect("a key serializes");
        published["kid"] = json!(id);
        Issuer {
            id: id.to_owned(),
            signing,
            published,
        }
    }

    pub fn keys(&self) -> Value {
        json!({"keys": [self.published.clone()]})
    }

    pub fn material(&self) -> String {
        self.published["x"].as_str().unwrap_or_default().to_owned()
    }

    pub fn mint(&self, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.id.clone());
        jsonwebtoken::encode(&header, claims, &self.signing).expect("the library signs")
    }

    pub fn minted_under(&self, header: &Value, claims: &Value) -> String {
        let input = Issuer::input(header, claims);
        let mark = jsonwebtoken::crypto::sign(input.as_bytes(), &self.signing, Algorithm::ES256)
            .expect("the library signs");
        format!("{input}.{mark}")
    }

    pub fn input(header: &Value, claims: &Value) -> String {
        let head = encode(&serde_json::to_vec(header).expect("a header serializes"));
        let body = encode(&serde_json::to_vec(claims).expect("claims serialize"));
        format!("{head}.{body}")
    }
}
