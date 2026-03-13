use async_trait::async_trait;
use fhir_core::security::bearer::KeySet;
use fhir_core::Error;
use serde_json::Value;
use std::collections::HashMap;
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
    pinned: HashMap<String, Vec<String>>,
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
            pinned: HashMap::new(),
            cached: RwLock::new(HashMap::new()),
        }
    }

    pub fn pinning<P>(mut self, issuer: &str, thumbprints: P) -> DiscoveredKeys
    where
        P: IntoIterator<Item = String>,
    {
        let held: Vec<String> = thumbprints
            .into_iter()
            .map(|thumbprint| thumbprint.trim().to_owned())
            .filter(|thumbprint| !thumbprint.is_empty())
            .collect();
        match held.is_empty() {
            true => self,
            false => {
                self.pinned.insert(origin_of(issuer), held);
                self
            }
        }
    }

    fn pins(&self, issuer: &str) -> Option<&Vec<String>> {
        self.pinned.get(&origin_of(issuer))
    }

    pub fn metadata_of(issuer: &str) -> String {
        format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        )
    }

    async fn fetched(&self, url: &str, pinned: bool) -> Result<Value, Error> {
        let failed = |reason: String| Error::Config(format!("issuer metadata: {reason}"));
        let secured = url.starts_with("https://");
        if !secured && !pinned {
            return Err(failed(
                "a plain address authenticates nobody; pin the issuer's keys or name an authenticated address"
                    .to_owned(),
            ));
        }
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .ok_or_else(|| failed("the address names no known scheme".to_owned()))?;
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_owned()),
        };
        let request = format!(
            "GET {path} HTTP/1.0\r\nhost: {authority}\r\naccept: application/json\r\nconnection: close\r\n\r\n"
        );
        let host = authority.split(':').next().unwrap_or(authority).to_owned();
        let target = match authority.contains(':') {
            true => authority.to_owned(),
            false => format!("{authority}:{}", if secured { 443 } else { 80 }),
        };

        let unreachable =
            |reason: String| Error::Unavailable(format!("the issuer cannot be reached: {reason}"));
        let exchange = async {
            let socket = TcpStream::connect(&target)
                .await
                .map_err(|error| unreachable(error.to_string()))?;
            match secured {
                true => authenticated(socket, &host, request.as_bytes()).await,
                false => plain(socket, request.as_bytes()).await,
            }
        };
        let raw = timeout(self.timeout, exchange)
            .await
            .map_err(|_| unreachable("it did not answer in time".to_owned()))??;
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

fn origin_of(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    match trimmed.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split('/').next().unwrap_or(rest);
            format!("{scheme}://{authority}")
        }
        None => trimmed.to_owned(),
    }
}

async fn plain(mut socket: TcpStream, request: &[u8]) -> Result<Vec<u8>, Error> {
    let failed = |reason: String| Error::Config(format!("issuer metadata: {reason}"));
    socket
        .write_all(request)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let mut raw = Vec::new();
    socket
        .read_to_end(&mut raw)
        .await
        .map_err(|error| failed(error.to_string()))?;
    Ok(raw)
}

pub const ENV_ISSUER_CA: &str = "FHIR_ISSUER_CA";

fn roots() -> Result<tokio_rustls::rustls::RootCertStore, Error> {
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let Ok(named) = std::env::var(ENV_ISSUER_CA) else {
        return Ok(roots);
    };
    let named = named.trim();
    if named.is_empty() {
        return Ok(roots);
    }
    let held = std::fs::read(named).map_err(|error| {
        Error::Config(format!("{ENV_ISSUER_CA} {named:?} cannot be read: {error}"))
    })?;
    let text = String::from_utf8(held)
        .map_err(|_| Error::Config(format!("{ENV_ISSUER_CA} {named:?} is not text")))?;
    let mut added = 0;
    for der in certificates_in(&text)? {
        roots
            .add(tokio_rustls::rustls::pki_types::CertificateDer::from(der))
            .map_err(|error| {
                Error::Config(format!(
                    "{ENV_ISSUER_CA} {named:?} holds a certificate this build will not trust: \
                     {error}"
                ))
            })?;
        added += 1;
    }
    if added == 0 {
        return Err(Error::Config(format!(
            "{ENV_ISSUER_CA} {named:?} holds no certificate"
        )));
    }
    Ok(roots)
}

fn certificates_in(text: &str) -> Result<Vec<Vec<u8>>, Error> {
    const OPENS: &str = "-----BEGIN CERTIFICATE-----";
    const CLOSES: &str = "-----END CERTIFICATE-----";
    let mut held = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(OPENS) {
        let body = &rest[at + OPENS.len()..];
        let Some(end) = body.find(CLOSES) else {
            return Err(Error::Config(
                "a certificate block is opened and not closed".to_owned(),
            ));
        };
        let encoded: String = body[..end]
            .chars()
            .filter(|held| !held.is_whitespace())
            .collect();
        let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &encoded)
            .map_err(|error| Error::Config(format!("a certificate is not base64: {error}")))?;
        held.push(der);
        rest = &body[end + CLOSES.len()..];
    }
    Ok(held)
}

async fn authenticated(socket: TcpStream, host: &str, request: &[u8]) -> Result<Vec<u8>, Error> {
    let failed = |reason: String| Error::Config(format!("issuer metadata: {reason}"));
    let roots = roots()?;
    let settings = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let named = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| failed("the issuer address names no server".to_owned()))?;
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(settings));
    let mut stream = connector.connect(named, socket).await.map_err(|error| {
        Error::Unavailable(format!(
            "the issuer was not authenticated: {error}; where it is served by an internal \
             authority, name that authority in {ENV_ISSUER_CA}"
        ))
    })?;
    stream
        .write_all(request)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|error| failed(error.to_string()))?;
    Ok(raw)
}

#[async_trait]
impl Keys for DiscoveredKeys {
    async fn keys(&self, issuer: &str) -> Result<KeySet, Error> {
        if let Some(held) = self.cached.read().await.get(issuer) {
            return Ok(held.clone());
        }
        let pins = self.pins(issuer);
        let metadata = self
            .fetched(&DiscoveredKeys::metadata_of(issuer), pins.is_some())
            .await?;
        let named = metadata
            .get("issuer")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Config("issuer metadata names no issuer".to_owned()))?;
        if origin_of(named) != origin_of(issuer) {
            return Err(Error::Config(
                "issuer metadata names another issuer".to_owned(),
            ));
        }
        let published = metadata
            .get("jwks_uri")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Config("issuer metadata names no key set".to_owned()))?;
        if origin_of(published) != origin_of(issuer) {
            return Err(Error::Config(
                "the key set is published away from the issuer".to_owned(),
            ));
        }
        let set = KeySet::parse(&self.fetched(published, pins.is_some()).await?)?;
        let set = match pins {
            None => set,
            Some(pins) => held_to(&set, pins)?,
        };
        self.cached
            .write()
            .await
            .insert(issuer.to_owned(), set.clone());
        Ok(set)
    }
}

fn held_to(set: &KeySet, pins: &[String]) -> Result<KeySet, Error> {
    let keys: Vec<_> = set
        .keys
        .iter()
        .filter(|key| {
            key.thumbprint()
                .is_some_and(|thumbprint| pins.iter().any(|pin| pin == &thumbprint))
        })
        .cloned()
        .collect();
    match keys.is_empty() {
        true => Err(Error::Config(
            "the issuer published no key this instance pins".to_owned(),
        )),
        false => Ok(KeySet { keys }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::security::fixture::Issuer;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn issuer(replies: usize) -> (String, String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let base = format!("http://{addr}");
        let published = base.clone();
        let document = Issuer::generate("one").keys();
        let pin = KeySet::parse(&document)
            .expect("a published key set")
            .thumbprints()
            .pop()
            .expect("one thumbprint");
        let handle = tokio::spawn(async move {
            for _ in 0..replies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = [0u8; 1024];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = match request.contains("openid-configuration") {
                    true => json!({
                        "issuer": published,
                        "jwks_uri": format!("{published}/keys"),
                    }),
                    false => document.clone(),
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
        (base, pin, handle)
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
        let (base, pin, handle) = issuer(2).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2)).pinning(&base, [pin]);
        let set = found.keys(&base).await.expect("the issuer publishes keys");
        assert_eq!(set.keys.len(), 1);
        assert_eq!(set.keys[0].id.as_deref(), Some("one"));
        assert_eq!(
            set.keys[0].algorithm,
            fhir_core::security::bearer::Algorithm::Es256
        );
        handle.abort();
    }

    #[tokio::test]
    async fn a_discovered_key_set_is_not_fetched_twice() {
        let (base, pin, handle) = issuer(2).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2)).pinning(&base, [pin]);
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
        let set = KeySet::parse(&Issuer::generate("held").keys()).expect("a configured key set");
        let held = HeldKeys::new(set.clone());
        assert_eq!(held.keys("https://issuer.example.org").await.unwrap(), set);
    }
}

#[cfg(test)]
mod pinning {
    use super::*;
    use fhir_core::security::fixture::Issuer;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct Answering {
        base: String,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for Answering {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    async fn answering(keys: Value, metadata: Option<Value>) -> Answering {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let base = format!("http://{addr}");
        let named = metadata.unwrap_or_else(
            || json!({ "issuer": base.clone(), "jwks_uri": format!("{base}/keys") }),
        );
        let keys = Arc::new(keys);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = [0u8; 1024];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = match request.contains("openid-configuration") {
                    true => named.to_string(),
                    false => keys.to_string(),
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Answering { base, handle }
    }

    fn thumbprint_of(document: &Value) -> String {
        KeySet::parse(document)
            .expect("a published key set")
            .thumbprints()
            .pop()
            .expect("one thumbprint")
    }

    #[tokio::test]
    async fn a_plain_address_authenticates_nobody_and_is_refused() {
        let published = Issuer::generate("one").keys();
        let issuer = answering(published, None).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2));
        let refused = found.keys(&issuer.base).await;
        assert!(refused.is_err(), "an unauthenticated fetch must be refused");
        let detail = refused
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(detail.contains("pin"), "{detail}");
    }

    #[tokio::test]
    async fn keys_pinned_by_thumbprint_are_taken_from_a_plain_address() {
        let published = Issuer::generate("one").keys();
        let pin = thumbprint_of(&published);
        let issuer = answering(published, None).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2)).pinning(&issuer.base, [pin]);
        let set = found.keys(&issuer.base).await.expect("the pinned key set");
        assert_eq!(set.keys.len(), 1);
        assert_eq!(set.keys[0].id.as_deref(), Some("one"));
    }

    #[tokio::test]
    async fn a_key_the_pin_does_not_name_is_refused() {
        let pin = thumbprint_of(&Issuer::generate("one").keys());
        let issuer = answering(Issuer::generate("two").keys(), None).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2)).pinning(&issuer.base, [pin]);
        let refused = found.keys(&issuer.base).await;
        assert!(refused.is_err(), "whoever answers must not choose the keys");
    }

    #[tokio::test]
    async fn metadata_naming_another_issuer_is_refused() {
        let published = Issuer::generate("one").keys();
        let pin = thumbprint_of(&published);
        let elsewhere = json!({
            "issuer": "http://elsewhere.invalid",
            "jwks_uri": "http://elsewhere.invalid/keys",
        });
        let issuer = answering(published, Some(elsewhere)).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2)).pinning(&issuer.base, [pin]);
        assert!(found.keys(&issuer.base).await.is_err());
    }

    #[tokio::test]
    async fn a_key_address_off_the_issuer_origin_is_refused() {
        let published = Issuer::generate("one").keys();
        let pin = thumbprint_of(&published);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let base = format!("http://{addr}");
        let strayed = json!({
            "issuer": base.clone(),
            "jwks_uri": "http://elsewhere.invalid/keys",
        });
        let issuer = answering(published, Some(strayed)).await;
        let found = DiscoveredKeys::new(Duration::from_secs(2)).pinning(&issuer.base, [pin]);
        let refused = found.keys(&issuer.base).await;
        assert!(
            refused.is_err(),
            "keys must come from the issuer's own origin"
        );
    }

    #[tokio::test]
    async fn an_authenticated_address_never_falls_back_to_a_plain_one() {
        let issuer = answering(Issuer::generate("one").keys(), None).await;
        let secured = issuer.base.replace("http://", "https://");
        let found = DiscoveredKeys::new(Duration::from_millis(500));
        assert!(
            found.keys(&secured).await.is_err(),
            "a plain answer must not satisfy an authenticated address"
        );
    }
}

#[cfg(test)]
mod trust_anchor_tests {
    use super::*;

    const ONE: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";

    #[test]
    fn a_file_of_no_certificates_holds_none() {
        assert!(certificates_in("nothing here")
            .expect("no block is no error")
            .is_empty());
    }

    #[test]
    fn each_block_is_decoded() {
        let held = certificates_in(&format!("{ONE}{ONE}")).expect("two blocks");
        assert_eq!(held.len(), 2);
        assert_eq!(held[0], vec![0x30, 0x82, 0x01]);
    }

    #[test]
    fn a_block_that_is_never_closed_is_refused() {
        let error = certificates_in("-----BEGIN CERTIFICATE-----\nMIIB\n")
            .expect_err("an unclosed block must fail");
        assert!(error.to_string().contains("not closed"), "{error}");
    }

    #[test]
    fn a_block_that_is_not_base64_is_refused() {
        let held = "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
        assert!(certificates_in(held).is_err());
    }

    #[test]
    fn an_instance_naming_no_anchor_trusts_the_public_roots_alone() {
        let held = std::env::var(ENV_ISSUER_CA);
        assert!(
            held.is_err() || held.as_deref() == Ok(""),
            "this test runs in an environment that names no anchor"
        );
        assert!(roots().expect("the public roots load").len() > 100);
    }
}

#[cfg(test)]
mod pinning_nothing {
    use super::*;

    #[test]
    fn pinning_no_thumbprint_is_not_pinning() {
        let held = DiscoveredKeys::new(std::time::Duration::from_secs(1))
            .pinning("https://issuer.example.org", Vec::<String>::new());
        assert!(
            held.pins("https://issuer.example.org").is_none(),
            "an empty list is an operator who named no pins, not one who pinned \
             a set nothing can satisfy"
        );
    }

    #[test]
    fn pinning_a_blank_thumbprint_is_not_pinning_either() {
        let held = DiscoveredKeys::new(std::time::Duration::from_secs(1))
            .pinning("https://issuer.example.org", ["  ".to_owned()]);
        assert!(held.pins("https://issuer.example.org").is_none());
    }

    #[test]
    fn a_named_thumbprint_is_still_pinned() {
        let held = DiscoveredKeys::new(std::time::Duration::from_secs(1))
            .pinning("https://issuer.example.org", ["abc".to_owned()]);
        assert_eq!(
            held.pins("https://issuer.example.org").map(Vec::len),
            Some(1)
        );
    }
}
