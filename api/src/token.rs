use fhir_core::Error;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::OnceLock;

const TAG: usize = 16;
const TOKEN: usize = 32;

pub const ENV_CONTINUATION_KEY: &str = "FHIR_CONTINUATION_KEY";

struct Key([u8; 32]);

impl Key {
    fn of(secret: &str) -> Key {
        Key(digest(secret.as_bytes(), b"continuation"))
    }

    fn generated() -> Key {
        let mut held = [0u8; 32];
        held[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        held[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Key(held)
    }

    fn from_env() -> Key {
        match std::env::var(ENV_CONTINUATION_KEY) {
            Ok(secret) if !secret.trim().is_empty() => Key::of(secret.trim()),
            _ => Key::generated(),
        }
    }

    fn number(&self, label: &str, scope: &str, offset: u64) -> u64 {
        let mut message = Vec::new();
        message.extend_from_slice(label.as_bytes());
        message.push(0);
        message.extend_from_slice(scope.as_bytes());
        message.push(0);
        message.extend_from_slice(&offset.to_be_bytes());
        let held = digest(&self.0, &message);
        u64::from_be_bytes(held[..8].try_into().unwrap_or_default())
    }
}

fn digest(secret: &[u8], message: &[u8]) -> [u8; 32] {
    let mut keyed =
        <Hmac<Sha256> as Mac>::new_from_slice(secret).unwrap_or_else(|_| Hmac::new_from_slice(&[0u8; 32]).expect("a fixed key is accepted"));
    keyed.update(message);
    let mut held = [0u8; 32];
    held.copy_from_slice(&keyed.finalize().into_bytes());
    held
}

fn keys() -> &'static Key {
    static KEYS: OnceLock<Key> = OnceLock::new();
    KEYS.get_or_init(Key::from_env)
}

fn tag(scope: &str, offset: u64) -> u64 {
    keys().number("tag", scope, offset)
}

fn mask(scope: &str) -> u64 {
    keys().number("mask", scope, 0)
}

pub fn encode(offset: usize, scope: &str) -> String {
    let offset = offset as u64;
    format!("{:016x}{:016x}", tag(scope, offset), offset ^ mask(scope))
}

pub fn decode(text: &str, scope: &str) -> Result<usize, Error> {
    let invalid = || Error::InvalidParameter(format!("ct {text:?}"));
    if text.len() != TOKEN || !text.is_char_boundary(TAG) {
        return Err(invalid());
    }
    let (carried, body) = text.split_at(TAG);
    let masked = u64::from_str_radix(body, 16).map_err(|_| invalid())?;
    let offset = masked ^ mask(scope);
    if carried != format!("{:016x}", tag(scope, offset)) {
        return Err(invalid());
    }
    usize::try_from(offset).map_err(|_| invalid())
}

pub fn scope(raw: Option<&str>) -> String {
    let Some(raw) = raw else { return String::new() };
    raw.split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("ct="))
        .collect::<Vec<&str>>()
        .join("&")
}

pub fn scope_of(self_url: &str) -> String {
    scope(self_url.split_once('?').map(|(_, query)| query))
}

pub fn with_token(self_url: &str, token: &str) -> String {
    let (path, query) = match self_url.split_once('?') {
        Some((path, query)) => (path, query),
        None => (self_url, ""),
    };
    let mut parts: Vec<String> = query
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("ct="))
        .map(str::to_owned)
        .collect();
    parts.push(format!("ct={token}"));
    format!("{path}?{}", parts.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issued(key: &Key, offset: u64, scope: &str) -> String {
        format!(
            "{:016x}{:016x}",
            key.number("tag", scope, offset),
            offset ^ key.number("mask", scope, 0)
        )
    }

    #[test]
    fn one_secret_gives_every_instance_the_same_tokens() {
        let here = Key::of("a-shared-continuation-key-value");
        let there = Key::of("a-shared-continuation-key-value");
        assert_eq!(issued(&here, 25, "_count=2"), issued(&there, 25, "_count=2"));

        let elsewhere = Key::of("another-key-entirely");
        assert_ne!(issued(&here, 25, "_count=2"), issued(&elsewhere, 25, "_count=2"));
    }

    #[test]
    fn an_instance_without_a_secret_keeps_its_tokens_to_itself() {
        let here = Key::generated();
        let there = Key::generated();
        assert_ne!(issued(&here, 25, "_count=2"), issued(&there, 25, "_count=2"));
    }

    #[test]
    fn a_token_round_trips_inside_its_own_query() {
        for offset in [0usize, 1, 25, 4096] {
            assert_eq!(decode(&encode(offset, "_count=2"), "_count=2").unwrap(), offset);
        }
    }

    #[test]
    fn an_edited_token_is_refused() {
        let token = encode(25, "_count=2");
        let last = token.chars().last().unwrap_or('0');
        let flipped = if last == 'f' { '0' } else { 'f' };
        let mangled = format!("{}{flipped}", &token[..token.len() - 1]);
        assert!(decode(&mangled, "_count=2").is_err());
        assert!(decode("", "_count=2").is_err());
        assert!(decode("zz", "_count=2").is_err());
        assert!(decode(&"z".repeat(TOKEN), "_count=2").is_err());
    }

    #[test]
    fn a_token_carries_no_meaning_in_another_query() {
        let token = encode(25, "family=Ann");
        assert!(decode(&token, "family=Bo").is_err());
        assert_ne!(encode(25, "family=Ann"), encode(25, "family=Bo"));
    }

    #[test]
    fn a_token_hides_the_offset_it_carries() {
        let token = encode(25, "_count=2");
        assert_eq!(token.len(), TOKEN);
        assert_ne!(&token[TAG..], format!("{:016x}", 25u64));
    }

    #[test]
    fn the_scope_of_a_page_drops_its_own_token() {
        assert_eq!(scope(Some("_count=2&ct=abc")), "_count=2".to_owned());
        assert_eq!(scope(None), String::new());
        assert_eq!(scope_of("http://localhost/Patient?_count=2&ct=abc"), "_count=2".to_owned());
        assert_eq!(scope_of("http://localhost/Patient"), String::new());
    }

    #[test]
    fn a_next_link_replaces_an_existing_token() {
        let url = with_token("http://localhost/_history?_count=2&ct=abc", "ff10");
        assert_eq!(url, "http://localhost/_history?_count=2&ct=ff10");
        assert_eq!(with_token("http://localhost/_history", "ff10"), "http://localhost/_history?ct=ff10");
    }
}
