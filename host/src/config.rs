use fhir_api::Authorization;
use fhir_core::security::bearer::KeySet;
use fhir_core::{Error, FhirVersion};
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

pub const DEFAULT_BIND: &str = "127.0.0.1:8080";
pub const DEFAULT_BACKEND: &str = "memory";
pub const DEFAULT_VERSION: &str = "R4";
pub const DEFAULT_DATABASE_URL: &str = "postgres://fhir:fhir@127.0.0.1:5432/fhir";

pub const ENV_BIND: &str = "FHIR_BIND";
pub const ENV_BACKEND: &str = "FHIR_BACKEND";
pub const ENV_VERSION: &str = "FHIR_VERSION";
pub const ENV_DATABASE_URL: &str = "FHIR_DATABASE_URL";
pub const ENV_DATA_DIR: &str = "FHIR_DATA_DIR";
pub const ENV_AUTH_ISSUER: &str = "FHIR_AUTH_ISSUER";
pub const ENV_AUTH_AUTHORIZE: &str = "FHIR_AUTH_AUTHORIZE";
pub const ENV_AUTH_TOKEN: &str = "FHIR_AUTH_TOKEN";
pub const ENV_AUTH_INTROSPECT: &str = "FHIR_AUTH_INTROSPECT";
pub const ENV_AUTH_SCOPES: &str = "FHIR_AUTH_SCOPES";
pub const ENV_AUTH_CAPABILITIES: &str = "FHIR_AUTH_CAPABILITIES";
pub const ENV_AUTH_KEYS: &str = "FHIR_AUTH_KEYS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Memory,
    Relational,
    Document,
}

impl FromStr for Backend {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        match value.trim() {
            "memory" => Ok(Backend::Memory),
            "relational" => Ok(Backend::Relational),
            "document" => Ok(Backend::Document),
            other => Err(Error::Config(format!(
                "unknown backend {other:?}; expected \"memory\", \"relational\" or \"document\""
            ))),
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Backend::Memory => write!(f, "memory"),
            Backend::Relational => write!(f, "relational"),
            Backend::Document => write!(f, "document"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: SocketAddr,
    pub backend: Backend,
    pub version: FhirVersion,
    pub database_url: String,
    pub data_dir: Option<PathBuf>,
    pub authorization: Option<Authorization>,
    pub keys: Option<KeySet>,
}

fn listed(env: &BTreeMap<String, String>, key: &str) -> Vec<String> {
    get(env, key)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn authorization(env: &BTreeMap<String, String>) -> Result<Option<Authorization>, Error> {
    let Some(issuer) = get(env, ENV_AUTH_ISSUER) else {
        return Ok(None);
    };
    let authorize = get(env, ENV_AUTH_AUTHORIZE).ok_or_else(|| {
        Error::Config(format!(
            "{ENV_AUTH_AUTHORIZE} is required once {ENV_AUTH_ISSUER} is set"
        ))
    })?;
    let token = get(env, ENV_AUTH_TOKEN).ok_or_else(|| {
        Error::Config(format!(
            "{ENV_AUTH_TOKEN} is required once {ENV_AUTH_ISSUER} is set"
        ))
    })?;
    let mut active = Authorization::new(issuer, authorize, token)
        .with_scopes(listed(env, ENV_AUTH_SCOPES))
        .with_capabilities(listed(env, ENV_AUTH_CAPABILITIES));
    if let Some(endpoint) = get(env, ENV_AUTH_INTROSPECT) {
        active = active.with_introspection(endpoint);
    }
    Ok(Some(active))
}

fn keys(env: &BTreeMap<String, String>) -> Result<Option<KeySet>, Error> {
    let Some(raw) = get(env, ENV_AUTH_KEYS) else {
        return Ok(None);
    };
    let document = serde_json::from_str(raw)
        .map_err(|_| Error::Config(format!("{ENV_AUTH_KEYS} is not a key set")))?;
    KeySet::parse(&document).map(Some)
}

impl Config {
    pub fn from_env() -> Result<Config, Error> {
        Config::parse(&std::env::vars().collect())
    }

    pub fn parse(env: &BTreeMap<String, String>) -> Result<Config, Error> {
        let bind_raw = get(env, ENV_BIND).unwrap_or(DEFAULT_BIND);
        let bind = bind_raw.parse::<SocketAddr>().map_err(|_| {
            Error::Config(format!("invalid {ENV_BIND} {bind_raw:?}; expected an address like \"127.0.0.1:8080\""))
        })?;

        let backend_raw = get(env, ENV_BACKEND).unwrap_or(DEFAULT_BACKEND);
        let backend = Backend::from_str(backend_raw).map_err(|_| {
            Error::Config(format!(
                "invalid {ENV_BACKEND} {backend_raw:?}; expected \"memory\", \"relational\" or \"document\""
            ))
        })?;

        let version_raw = get(env, ENV_VERSION).unwrap_or(DEFAULT_VERSION);
        let version = version_raw.parse::<FhirVersion>().map_err(|_| {
            Error::Config(format!(
                "invalid {ENV_VERSION} {version_raw:?}; expected STU3, R4, R4B or R5"
            ))
        })?;

        let database_url = get(env, ENV_DATABASE_URL)
            .map(str::to_owned)
            .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_owned());

        let data_dir = get(env, ENV_DATA_DIR).map(PathBuf::from);

        match backend {
            Backend::Memory => {}
            Backend::Relational => {
                if database_url.is_empty() {
                    return Err(Error::Config(format!(
                        "{ENV_DATABASE_URL} is required for the relational backend"
                    )));
                }
            }
            Backend::Document => {
                if data_dir.is_none() {
                    return Err(Error::Config(format!(
                        "{ENV_DATA_DIR} is required for the document backend"
                    )));
                }
            }
        }

        Ok(Config {
            bind,
            backend,
            version,
            database_url,
            data_dir,
            authorization: authorization(env)?,
            keys: keys(env)?,
        })
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "backend={} version={} bind={} data_dir={:?} database_url=redacted authorization={}",
            self.backend,
            self.version,
            self.bind,
            self.data_dir,
            match self.authorization {
                Some(_) => "active",
                None => "none",
            }
        )
    }
}

fn get<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_empty() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn an_unset_issuer_leaves_the_instance_unsecured() {
        let config = Config::parse(&env_empty()).expect("defaults must parse");
        assert_eq!(config.authorization, None);
    }

    #[test]
    fn an_issuer_without_its_endpoints_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_AUTH_ISSUER.to_owned(), "https://issuer.example.org".to_owned());
        let error = Config::parse(&env).expect_err("a half-set authorization must fail");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains(ENV_AUTH_AUTHORIZE));
        env.insert(ENV_AUTH_AUTHORIZE.to_owned(), "https://issuer.example.org/a".to_owned());
        let error = Config::parse(&env).expect_err("a missing token endpoint must fail");
        assert!(error.to_string().contains(ENV_AUTH_TOKEN));
    }

    #[test]
    fn the_configured_authorization_is_the_one_reported() {
        let mut env = env_empty();
        env.insert(ENV_AUTH_ISSUER.to_owned(), "https://issuer.example.org".to_owned());
        env.insert(ENV_AUTH_AUTHORIZE.to_owned(), "https://issuer.example.org/a".to_owned());
        env.insert(ENV_AUTH_TOKEN.to_owned(), "https://issuer.example.org/t".to_owned());
        env.insert(ENV_AUTH_INTROSPECT.to_owned(), "https://issuer.example.org/i".to_owned());
        env.insert(ENV_AUTH_SCOPES.to_owned(), "system/*.read, system/*.write".to_owned());
        env.insert(ENV_AUTH_CAPABILITIES.to_owned(), "client-confidential-symmetric".to_owned());
        let config = Config::parse(&env).expect("a complete authorization must parse");
        let active = config.authorization.clone().expect("authorization must be active");
        assert_eq!(active.issuer, "https://issuer.example.org");
        assert_eq!(active.introspect.as_deref(), Some("https://issuer.example.org/i"));
        assert_eq!(active.scopes, vec!["system/*.read".to_owned(), "system/*.write".to_owned()]);
        assert_eq!(active.capabilities, vec!["client-confidential-symmetric".to_owned()]);
        assert!(config.to_string().contains("authorization=active"));
    }

    #[test]
    fn defaults_are_valid() {
        let config = Config::parse(&env_empty()).expect("defaults must parse");
        assert_eq!(config.backend, Backend::Memory);
        assert_eq!(config.version, FhirVersion::R4);
        assert_eq!(config.bind, "127.0.0.1:8080".parse::<SocketAddr>().unwrap());
        assert_eq!(config.database_url, DEFAULT_DATABASE_URL);
        assert_eq!(config.data_dir, None);
    }

    #[test]
    fn environment_overrides_defaults() {
        let mut env = env_empty();
        env.insert(ENV_BIND.to_owned(), "0.0.0.0:9090".to_owned());
        env.insert(ENV_VERSION.to_owned(), "R5".to_owned());
        env.insert(ENV_BACKEND.to_owned(), "document".to_owned());
        env.insert(ENV_DATA_DIR.to_owned(), "/var/lib/fhir".to_owned());
        let config = Config::parse(&env).expect("valid env must parse");
        assert_eq!(config.bind.to_string(), "0.0.0.0:9090");
        assert_eq!(config.version, FhirVersion::R5);
        assert_eq!(config.backend, Backend::Document);
        assert_eq!(config.data_dir, Some(PathBuf::from("/var/lib/fhir")));
    }

    #[test]
    fn memory_backend_requires_no_extra_fields() {
        let mut env = env_empty();
        env.insert(ENV_BACKEND.to_owned(), "memory".to_owned());
        let config = Config::parse(&env).expect("memory backend must parse");
        assert_eq!(config.backend, Backend::Memory);
        assert_eq!(config.database_url, DEFAULT_DATABASE_URL);
    }

    #[test]
    fn values_are_trimmed() {
        let mut env = env_empty();
        env.insert(ENV_BACKEND.to_owned(), "  memory  ".to_owned());
        env.insert(ENV_BIND.to_owned(), " 127.0.0.1:8080 ".to_owned());
        let config = Config::parse(&env).expect("trimmed env must parse");
        assert_eq!(config.backend, Backend::Memory);
        assert_eq!(config.bind.to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn unknown_backend_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_BACKEND.to_owned(), "nosql".to_owned());
        let error = Config::parse(&env).expect_err("unknown backend must fail");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains("FHIR_BACKEND"));
        assert!(error.to_string().contains("memory"));
        assert!(error.to_string().contains("relational"));
        assert!(error.to_string().contains("document"));
    }

    #[test]
    fn invalid_version_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_VERSION.to_owned(), "2".to_owned());
        let error = Config::parse(&env).expect_err("invalid version must fail");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains("FHIR_VERSION"));
    }

    #[test]
    fn invalid_bind_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_BIND.to_owned(), "not-an-address".to_owned());
        let error = Config::parse(&env).expect_err("invalid bind must fail");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains("FHIR_BIND"));
    }

    #[test]
    fn document_backend_requires_data_dir() {
        let mut env = env_empty();
        env.insert(ENV_BACKEND.to_owned(), "document".to_owned());
        let error = Config::parse(&env).expect_err("document backend without a data dir must fail");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains("FHIR_DATA_DIR"));
    }

    #[test]
    fn empty_values_fall_back_to_defaults() {
        let mut env = env_empty();
        env.insert(ENV_BIND.to_owned(), String::new());
        env.insert(ENV_BACKEND.to_owned(), "   ".to_owned());
        let config = Config::parse(&env).expect("empty values must fall back to defaults");
        assert_eq!(config.backend, Backend::Memory);
        assert_eq!(config.bind.to_string(), DEFAULT_BIND);
    }
}