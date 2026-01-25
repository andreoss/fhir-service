use async_trait::async_trait;
use fhir_core::security::bearer::KeySet;
use fhir_core::Error;
use std::collections::HashMap;
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::timeout;

#[async_trait]
pub trait Keys: Send + Sync {
    async fn keys(&self, issuer: &str) -> Result<KeySet, Error>;
}

pub struct HeldKeys {
    set: KeySet,
}

pub struct DiscoveredKeys {
    timeout: Duration,
    cached: RwLock<HashMap<String, KeySet>>,
}

impl HeldKeys {
    pub fn new(set: KeySet) -> HeldKeys {
        HeldKeys { set }
    }
}

#[async_trait]
impl Keys for HeldKeys {
    async fn keys(&self, _issuer: &str) -> Result<KeySet, Error> {
        Ok(self.set.clone())
    }
}

impl DiscoveredKeys {
    pub fn new(timeout: Duration) -> DiscoveredKeys {
        DiscoveredKeys {
            timeout,
            cached: RwLock::new(HashMap::new()),
        }
    }

    pub fn metadata_of(issuer: &str) -> String {
        format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        )
    }

    async fn fetched(&self, url: &str) -> Result<Value, Error> {
        let failed = |reason: String| Error::Config(format!("issuer metadata: {reason}"));
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| failed("only a plain address is fetched by this build".to_owned()))?;
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_owned()),
        };
        let request = format!(
            "GET {path} HTTP/1.0\r\nhost: {authority}\r\naccept: application/json\r\nconnection: close\r\n\r\n"
        );
        let exchange = async {
            let mut socket = TcpStream::connect(authority)
                .await
                .map_err(|error| failed(error.to_string()))?;
            socket
                .write_all(request.as_bytes())
                .await
                .map_err(|error| failed(error.to_string()))?;
            let mut raw = Vec::new();
            socket
                .read_to_end(&mut raw)
                .await
                .map_err(|error| failed(error.to_string()))?;
            Ok::<Vec<u8>, Error>(raw)
        };
        let raw = timeout(self.timeout, exchange)
            .await
            .map_err(|_| failed("the issuer did not answer in time".to_owned()))??;
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| failed("truncated reply".to_owned()))?;
        if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
            return Err(failed("the issuer refused the request".to_owned()));
        }
        serde_json::from_str(body).map_err(|_| failed("the reply is not a document".to_owned()))
    }
}

#[async_trait]
impl Keys for DiscoveredKeys {
    async fn keys(&self, issuer: &str) -> Result<KeySet, Error> {
        if let Some(held) = self.cached.read().await.get(issuer) {
            return Ok(held.clone());
        }
        let metadata = self.fetched(&DiscoveredKeys::metadata_of(issuer)).await?;
        let published = metadata
            .get("jwks_uri")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Config("issuer metadata names no key set".to_owned()))?;
        let set = KeySet::parse(&self.fetched(published).await?)?;
        self.cached
            .write()
            .await
            .insert(issuer.to_owned(), set.clone());
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::security::bearer::encode;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn issuer(replies: usize) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let base = format!("http://{addr}");
        let published = base.clone();
        let handle = tokio::spawn(async move {
            for _ in 0..replies {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let mut buffer = [0u8; 1024];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = match request.contains("openid-configuration") {
                    true => json!({
                        "issuer": published,
                        "jwks_uri": format!("{published}/keys"),
                    }),
                    false => json!({"keys": [
                        {"kty": "oct", "kid": "one", "alg": "HS256", "k": encode(b"a-secret")}
                    ]}),
                }
                .to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (base, handle)
    }

    #[tokio::test]
    async fn the_metadata_address_follows_the_issuer() {
        assert_eq!(
            DiscoveredKeys::metadata_of("https://issuer.example.org/"),
            "https://issuer.example.org/.well-known/openid-configuration"
        );
        assert_eq!(
            DiscoveredKeys::metadata_of("https://issuer.example.org"),
            "https://issuer.example.org/.well-known/openid-configuration"
        );
    }

    #[tokio::test]
    async fn keys_come_from_the_issuer_the_metadata_points_at() {
        let (base, handle) = issuer(2).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2));
        let set = found.keys(&base).await.expect("the issuer publishes keys");
        assert_eq!(set.keys.len(), 1);
        assert_eq!(set.keys[0].id.as_deref(), Some("one"));
        assert!(!set.keys[0].secret.is_empty());
        handle.abort();
    }

    #[tokio::test]
    async fn a_discovered_key_set_is_not_fetched_twice() {
        let (base, handle) = issuer(2).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2));
        let first = found.keys(&base).await.expect("first discovery");
        handle.abort();
        let second = found.keys(&base).await.expect("the cached key set");
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn an_unreachable_issuer_yields_no_keys() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let found = DiscoveredKeys::new(Duration::from_millis(200));
        assert!(found.keys(&format!("http://{addr}")).await.is_err());
    }

    #[tokio::test]
    async fn keys_held_from_configuration_need_no_issuer() {
        let set = KeySet::parse(&json!({"keys": [
            {"kty": "oct", "kid": "held", "alg": "HS256", "k": encode(b"a-secret")}
        ]}))
        .expect("a configured key set");
        let held = HeldKeys::new(set.clone());
        assert_eq!(held.keys("https://issuer.example.org").await.unwrap(), set);
    }
}
