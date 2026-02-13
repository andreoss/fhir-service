use fhir_api::Authorization;
use fhir_core::security::bearer::KeySet;
use fhir_core::{Error, FhirVersion};
use fhir_telemetry::Scrape;
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

pub const DEFAULT_BIND: &str = "127.0.0.1:8080";
pub const DEFAULT_BACKEND: &str = "memory";
pub const DEFAULT_VERSION: &str = "R4";
pub const DEFAULT_DATABASE_URL: &str = "postgres://fhir:fhir@127.0.0.1:5432/fhir";

pub const DEFAULT_DOCUMENT_URL: &str = "mongodb://127.0.0.1:27017/?directConnection=true";

pub const ENV_BIND: &str = "FHIR_BIND";
pub const ENV_BACKEND: &str = "FHIR_BACKEND";
pub const ENV_VERSION: &str = "FHIR_VERSION";
pub const ENV_DATABASE_URL: &str = "FHIR_DATABASE_URL";
pub const ENV_DATA_DIR: &str = "FHIR_DATA_DIR";

pub const ENV_TERMINOLOGY_DIR: &str = "FHIR_TERMINOLOGY_DIR";

pub const ENV_DOCUMENT_URL: &str = "FHIR_DOCUMENT_URL";
pub const ENV_AUTH_ISSUER: &str = "FHIR_AUTH_ISSUER";
pub const ENV_AUTH_AUTHORIZE: &str = "FHIR_AUTH_AUTHORIZE";
pub const ENV_AUTH_TOKEN: &str = "FHIR_AUTH_TOKEN";
pub const ENV_AUTH_INTROSPECT: &str = "FHIR_AUTH_INTROSPECT";
pub const ENV_AUTH_SCOPES: &str = "FHIR_AUTH_SCOPES";
pub const ENV_AUTH_CAPABILITIES: &str = "FHIR_AUTH_CAPABILITIES";
pub const ENV_AUTH_KEYS: &str = "FHIR_AUTH_KEYS";
pub const ENV_METRICS_CREDENTIAL: &str = "FHIR_METRICS_CREDENTIAL";

pub const ENV_STORE_CONNECTIONS: &str = "FHIR_STORE_CONNECTIONS";

pub const DEFAULT_STORE_CONNECTIONS: u32 = 16;

pub const MOST_STORE_CONNECTIONS: u32 = 1_024;

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
    pub document_url: String,
    pub data_dir: Option<PathBuf>,
    pub terminology_dir: Option<PathBuf>,
    pub connections: u32,
    pub authorization: Option<Authorization>,
    pub keys: Option<KeySet>,
    pub scrape: Scrape,
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

fn scrape(env: &BTreeMap<String, String>) -> Result<Scrape, Error> {
    match get(env, ENV_METRICS_CREDENTIAL) {
        Some(credential) => Scrape::guarded(credential).map_err(|_| {
            Error::Config(format!(
                "{ENV_METRICS_CREDENTIAL} is at least sixteen characters"
            ))
        }),
        None => Ok(Scrape::closed()),
    }
}

impl Config {
    pub fn entries(&self) -> usize {
        (self.connections / 2).max(1) as usize
    }

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

        let document_url = get(env, ENV_DOCUMENT_URL)
            .map(str::to_owned)
            .unwrap_or_else(|| DEFAULT_DOCUMENT_URL.to_owned());

        let data_dir = get(env, ENV_DATA_DIR).map(PathBuf::from);

        let terminology_dir = get(env, ENV_TERMINOLOGY_DIR).map(PathBuf::from);

        let connections = match get(env, ENV_STORE_CONNECTIONS) {
            Some(raw) => match raw.parse::<u32>() {
                Ok(held) if (1..=MOST_STORE_CONNECTIONS).contains(&held) => held,
                _ => {
                    return Err(Error::Config(format!(
                        "invalid {ENV_STORE_CONNECTIONS} {raw:?}; expected 1 to {MOST_STORE_CONNECTIONS}"
                    )))
                }
            },
            None => DEFAULT_STORE_CONNECTIONS,
        };

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
                if document_url.is_empty() {
                    return Err(Error::Config(format!(
                        "{ENV_DOCUMENT_URL} is required for the document backend"
                    )));
                }
            }
        }

        Ok(Config {
            bind,
            backend,
            version,
            database_url,
            document_url,
            data_dir,
            terminology_dir,
            connections,
            authorization: authorization(env)?,
            keys: keys(env)?,
            scrape: scrape(env)?,
        })
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "backend={} version={} bind={} data_dir={:?} database_url=redacted authorization={} metrics={}",
            self.backend,
            self.version,
            self.bind,
            self.data_dir,
            match self.authorization {
                Some(_) => "active",
                None => "none",
            },
            match self.scrape.serves() {
                true => "guarded",
                false => "restricted",
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
    fn the_entry_limit_is_chosen_with_the_connection_count() {
        let mut env = env_empty();
        let config = Config::parse(&env).expect("defaults must parse");
        assert_eq!(config.connections, DEFAULT_STORE_CONNECTIONS);
        assert_eq!(config.entries(), 8);

        env.insert(ENV_STORE_CONNECTIONS.to_owned(), "2".to_owned());
        let narrow = Config::parse(&env).expect("a smaller pool must parse");
        assert_eq!(narrow.connections, 2);
        assert_eq!(narrow.entries(), 1);

        env.insert(ENV_STORE_CONNECTIONS.to_owned(), "1".to_owned());
        assert_eq!(Config::parse(&env).expect("one connection parses").entries(), 1);
    }

    #[test]
    fn a_connection_count_outside_the_range_fails_fast() {
        let mut env = env_empty();
        for raw in ["0", "none", "-1", "100000"] {
            env.insert(ENV_STORE_CONNECTIONS.to_owned(), raw.to_owned());
            let error = Config::parse(&env).expect_err("an unusable count must fail");
            assert!(error.to_string().contains(ENV_STORE_CONNECTIONS), "{raw}");
        }
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
        env.insert(ENV_DOCUMENT_URL.to_owned(), "mongodb://127.0.0.1:27018".to_owned());
        let config = Config::parse(&env).expect("valid env must parse");
        assert_eq!(config.bind.to_string(), "0.0.0.0:9090");
        assert_eq!(config.version, FhirVersion::R5);
        assert_eq!(config.backend, Backend::Document);
        assert_eq!(config.document_url, "mongodb://127.0.0.1:27018");
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
    fn document_backend_requires_an_engine_address() {
        let mut env = env_empty();
        env.insert(ENV_BACKEND.to_owned(), "document".to_owned());
        assert_eq!(
            Config::parse(&env).expect("a default address is enough").document_url,
            DEFAULT_DOCUMENT_URL
        );
        env.insert(ENV_DOCUMENT_URL.to_owned(), "  ".to_owned());
        let config = Config::parse(&env).expect("a blank address falls back");
        assert_eq!(config.document_url, DEFAULT_DOCUMENT_URL);
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
#[cfg(test)]
mod supplied_tests {
    use super::*;

    #[test]
    fn an_unset_directory_leaves_the_published_content_alone() {
        let config = Config::parse(&BTreeMap::new()).expect("defaults must parse");
        assert_eq!(config.terminology_dir, None);
        let held = crate::terminology::loaded(&config).expect("no directory loads nothing");
        assert_eq!(held.systems().len(), fhir_core::Catalogue::of(config.version).systems().len());
    }

    #[test]
    fn a_directory_of_code_systems_is_loaded_over_the_published_content() {
        let mut env = BTreeMap::new();
        let dir = std::path::Path::new("../../scratch/terminology-supplied");
        std::fs::create_dir_all(dir).expect("the directory is writable");
        std::fs::write(
            dir.join("held.json"),
            br#"{"resourceType":"CodeSystem","url":"urn:supplied","version":"1",
                 "content":"complete","concept":[{"code":"a","concept":[{"code":"b"}]}]}"#,
        )
        .expect("the file is writable");
        std::fs::write(dir.join("ignored.txt"), b"not json").expect("the file is writable");
        env.insert(ENV_TERMINOLOGY_DIR.to_owned(), dir.display().to_string());
        let config = Config::parse(&env).expect("a named directory parses");
        let held = crate::terminology::loaded(&config).expect("the directory loads");
        assert!(held.system("urn:supplied", Some("1")).is_some());
        let under: Vec<String> = held
            .descendants(Some("urn:supplied"), "a")
            .into_iter()
            .map(|concept| concept.code)
            .collect();
        assert_eq!(under, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn a_directory_that_is_not_there_fails_fast() {
        let mut env = BTreeMap::new();
        env.insert(ENV_TERMINOLOGY_DIR.to_owned(), "no/such/place".to_owned());
        let config = Config::parse(&env).expect("a named directory parses");
        assert!(crate::terminology::loaded(&config).is_err());
    }
}
