use async_trait::async_trait;
use fhir_core::security::bearer::Claims;
use fhir_core::Error;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::timeout;

const HELD_AT_MOST: i64 = 300;

#[async_trait]
pub trait Opaque: Send + Sync {
    async fn claims(&self, token: &str) -> Result<Claims, Error>;
}

pub struct Introspection {
    endpoint: String,
    client: String,
    secret: String,
    issuer: String,
    audience: Option<String>,
    timeout: Duration,
    cached: RwLock<HashMap<String, (Claims, i64)>>,
}

fn local(authority: &str) -> bool {
    let host = authority.split(':').next().unwrap_or(authority);
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
}

fn split(url: &str) -> Result<(bool, String, String), Error> {
    let refused = |reason: &str| Error::Config(format!("the introspection endpoint {reason}"));
    let secured = url.starts_with("https://");
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| refused("names no known scheme"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority.to_owned(), format!("/{path}")),
        None => (rest.to_owned(), "/".to_owned()),
    };
    if !secured && !local(&authority) {
        return Err(refused(
            "is a plain address: a token posted over one is a token given away",
        ));
    }
    Ok((secured, authority, path))
}

impl Introspection {
    pub fn new(
        endpoint: &str,
        client: &str,
        secret: &str,
        issuer: &str,
        audience: Option<&str>,
        patience: Duration,
    ) -> Result<Introspection, Error> {
        split(endpoint)?;
        Ok(Introspection {
            endpoint: endpoint.to_owned(),
            client: client.to_owned(),
            secret: secret.to_owned(),
            issuer: issuer.to_owned(),
            audience: audience.map(str::to_owned),
            timeout: patience,
            cached: RwLock::new(HashMap::new()),
        })
    }

    fn key(token: &str) -> String {
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update(token.as_bytes());
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn asked(&self, token: &str) -> Result<Value, Error> {
        let (secured, authority, path) = split(&self.endpoint)?;
        let credential = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!("{}:{}", self.client, self.secret),
        );
        let body = format!("token={}&token_type_hint=access_token", encoded(token));
        let request = format!(
            "POST {path} HTTP/1.0\r\nhost: {authority}\r\nauthorization: Basic {credential}\r\n\
             content-type: application/x-www-form-urlencoded\r\naccept: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let host = authority.split(':').next().unwrap_or(&authority).to_owned();
        let target = match authority.contains(':') {
            true => authority.clone(),
            false => format!("{authority}:{}", if secured { 443 } else { 80 }),
        };
        let unreachable = |reason: String| {
            Error::Unauthenticated(format!(
                "the issuer could not be asked about this token: {reason}"
            ))
        };
        let exchange = async {
            let socket = TcpStream::connect(&target)
                .await
                .map_err(|error| unreachable(error.to_string()))?;
            match secured {
                true => crate::discovery::authenticated(socket, &host, request.as_bytes()).await,
                false => crate::discovery::plain(socket, request.as_bytes()).await,
            }
        };
        let raw = timeout(self.timeout, exchange)
            .await
            .map_err(|_| unreachable("it did not answer in time".to_owned()))?
            .map_err(|error| unreachable(error.to_string()))?;
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| unreachable("the reply was truncated".to_owned()))?;
        if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
            return Err(unreachable("it refused the question".to_owned()));
        }
        serde_json::from_str(body)
            .map_err(|_| unreachable("the reply is not a document".to_owned()))
    }
}

fn encoded(raw: &str) -> String {
    raw.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[async_trait]
impl Opaque for Introspection {
    async fn claims(&self, token: &str) -> Result<Claims, Error> {
        let now = crate::access::now();
        let key = Introspection::key(token);
        if let Some((held, until)) = self.cached.read().await.get(&key) {
            if now < *until {
                return Ok(held.clone());
            }
        }
        let reply = self.asked(token).await?;
        let claims = Claims::introspected(&reply, &self.issuer, self.audience.as_deref(), now)?;
        let until = match claims.expires_at {
            Some(at) => at.min(now + HELD_AT_MOST),
            None => now + HELD_AT_MOST,
        };
        if until > now {
            self.cached
                .write()
                .await
                .insert(key, (claims.clone(), until));
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_endpoint_that_is_not_this_instance_is_refused() {
        let made = Introspection::new(
            "http://issuer.example.org/introspect",
            "an-app",
            "a-secret",
            "https://issuer.example.org",
            None,
            Duration::from_secs(2),
        );
        let error = made.err().expect("a plain address must be refused");
        assert!(error.to_string().contains("plain address"), "{error}");

        assert!(Introspection::new(
            "https://issuer.example.org/introspect",
            "an-app",
            "a-secret",
            "https://issuer.example.org",
            None,
            Duration::from_secs(2),
        )
        .is_ok());
        assert!(Introspection::new(
            "http://127.0.0.1:9/introspect",
            "an-app",
            "a-secret",
            "https://issuer.example.org",
            None,
            Duration::from_secs(2),
        )
        .is_ok());
    }

    #[test]
    fn a_token_is_never_the_key_it_is_cached_under() {
        let key = Introspection::key("a-token-nobody-should-see");
        assert_eq!(key.len(), 64);
        assert!(!key.contains("token"));
        assert_eq!(key, Introspection::key("a-token-nobody-should-see"));
    }

    #[tokio::test]
    async fn an_endpoint_that_does_not_answer_refuses_the_request() {
        let held = Introspection::new(
            "http://127.0.0.1:9/introspect",
            "an-app",
            "a-secret",
            "https://issuer.example.org",
            None,
            Duration::from_millis(200),
        )
        .expect("a loopback endpoint is allowed");
        let error = held.claims("a-token").await.expect_err("nothing answers");
        assert!(matches!(error, Error::Unauthenticated(_)), "{error:?}");
    }
}
