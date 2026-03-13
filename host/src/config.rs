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
pub const ENV_AUTH_AUDIENCE: &str = "FHIR_AUTH_AUDIENCE";
pub const ENV_AUTH_AUTHORIZE: &str = "FHIR_AUTH_AUTHORIZE";
pub const ENV_AUTH_TOKEN: &str = "FHIR_AUTH_TOKEN";
pub const ENV_AUTH_INTROSPECT: &str = "FHIR_AUTH_INTROSPECT";
pub const ENV_AUTH_SCOPES: &str = "FHIR_AUTH_SCOPES";
pub const ENV_AUTH_CAPABILITIES: &str = "FHIR_AUTH_CAPABILITIES";
pub const ENV_AUTH_JWKS: &str = "FHIR_AUTH_JWKS";
pub const ENV_AUTH_KEYS: &str = "FHIR_AUTH_KEYS";

const OPENID_CAPABILITY: &str = "sso-openid-connect";
pub const ENV_METRICS_CREDENTIAL: &str = "FHIR_METRICS_CREDENTIAL";

pub const ENV_ISSUER_PINS: &str = "FHIR_ISSUER_PINS";

pub const ENV_STORE_CONNECTIONS: &str = "FHIR_STORE_CONNECTIONS";

pub const DEFAULT_STORE_CONNECTIONS: u32 = 16;

pub const MOST_STORE_CONNECTIONS: u32 = 1_024;

pub const ENV_STORE_WAIT: &str = "FHIR_STORE_WAIT_MILLIS";

pub const ENV_VERSIONING: &str = "FHIR_VERSIONING";

pub const ENV_PROFILE_VALIDATION: &str = "FHIR_PROFILE_VALIDATION";

pub const ENV_ROLES: &str = "FHIR_ROLES";

pub const ENV_AT_ONCE: &str = "FHIR_REQUESTS_AT_ONCE";

pub const ENV_RETENTION: &str = "FHIR_RETENTION";

pub const ENV_CONDITIONAL_DELETE: &str = "FHIR_CONDITIONAL_DELETE";

pub const ENV_CREATE_ON_UPDATE: &str = "FHIR_CREATE_ON_UPDATE";

pub const ENV_INCLUDE_DEPTH: &str = "FHIR_INCLUDE_DEPTH";

pub const ENV_ALLOWED_PROFILES: &str = "FHIR_ALLOWED_PROFILES";

pub const ENV_SECURITY_HEADERS: &str = "FHIR_SECURITY_HEADERS";

pub const ENV_TYPES: &str = "FHIR_TYPES";

pub const ENV_PARAMETERS: &str = "FHIR_PARAMETERS";

pub const ENV_TRUST_PROXY: &str = "FHIR_TRUST_PROXY";

pub const ENV_REFERENCES: &str = "FHIR_REFERENCES";

pub const ENV_PAGE_SIZE: &str = "FHIR_PAGE_SIZE";

pub const ENV_PAGE_LIMIT: &str = "FHIR_PAGE_LIMIT";

pub const ENV_DEFAULT_SORT: &str = "FHIR_DEFAULT_SORT";

pub const ENV_DEFAULT_TOTAL: &str = "FHIR_DEFAULT_TOTAL";

pub const ENV_MAX_BODY: &str = "FHIR_MAX_BODY";

pub const ENV_MAX_ENTRIES: &str = "FHIR_MAX_ENTRIES";

pub const ENV_UNCHANGED: &str = "FHIR_UNCHANGED";

pub const ENV_DEFAULT_FORMAT: &str = "FHIR_DEFAULT_FORMAT";

pub const ENV_PRELOAD: &str = "FHIR_PRELOAD";

pub const ENV_RESET: &str = "FHIR_RESET";

pub const ENV_OTLP: &str = "FHIR_OTLP";

pub const ENV_ALERT: &str = "FHIR_ALERT";

pub const ENV_ACCESS_POLICY: &str = "FHIR_ACCESS_POLICY";

pub const ENV_TENANCY: &str = "FHIR_TENANCY";

pub const ENV_TENANT_CLAIM: &str = "FHIR_TENANT_CLAIM";

pub const ENV_ADMINISTRATION: &str = "FHIR_ADMINISTRATION";

pub const ENV_VERSIONS: &str = "FHIR_VERSIONS";

pub const MOST_INCLUDE_DEPTH: usize = 32;

pub const MOST_AT_ONCE: usize = 100_000;

pub const DEFAULT_STORE_WAIT: std::time::Duration = std::time::Duration::from_millis(1_000);

pub const MOST_STORE_WAIT_MILLIS: u64 = 60_000;

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
    pub wait: std::time::Duration,
    pub authorization: Option<Authorization>,
    pub keys: Option<KeySet>,
    pub pins: Vec<String>,
    pub scrape: Scrape,
    pub versioning: fhir_api::Versioning,
    pub profiles: fhir_api::OnWrite,
    pub roles: fhir_api::Roles,
    pub throttle: fhir_api::Throttle,
    pub retention: fhir_jobs::Retention,
    pub capabilities: fhir_api::Capabilities,
    pub allowed_profiles: fhir_api::AllowedProfiles,

    pub versions: Vec<FhirVersion>,
    pub administration: fhir_api::Administration,
    pub tenancy: fhir_api::Tenancy,
    pub policies: fhir_api::Policies,
    pub security_headers: fhir_api::SecurityHeaders,
    pub alert: Option<crate::alarm::Called>,
    pub collector: Option<crate::otlp::Collector>,
    pub preload: Option<std::path::PathBuf>,
    pub reset: fhir_api::Resettable,
    pub unchanged: fhir_api::Unchanged,
    pub default_format: fhir_api::MediaType,
    pub paging: fhir_api::Paging,
    pub limits: fhir_api::Limits,
    pub forwarding: fhir_api::Forwarding,
    pub references: fhir_api::References,
    pub restricted: fhir_api::Restricted,
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
    let audience = get(env, ENV_AUTH_AUDIENCE).ok_or_else(|| {
        Error::Config(format!(
            "{ENV_AUTH_AUDIENCE} is required once {ENV_AUTH_ISSUER} is set: a token minted for another relying party of the same issuer is otherwise accepted"
        ))
    })?;
    let capabilities = listed(env, ENV_AUTH_CAPABILITIES);
    let mut active = Authorization::new(issuer, authorize, token)
        .with_audience(audience)
        .with_scopes(listed(env, ENV_AUTH_SCOPES))
        .with_capabilities(capabilities.clone());
    if let Some(endpoint) = get(env, ENV_AUTH_INTROSPECT) {
        active = active.with_introspection(endpoint);
    }
    match get(env, ENV_AUTH_JWKS) {
        Some(address) => active = active.with_jwks(address),
        None if capabilities.iter().any(|named| named == OPENID_CAPABILITY) => {
            return Err(Error::Config(format!(
                "{ENV_AUTH_JWKS} is required once {ENV_AUTH_CAPABILITIES} names {OPENID_CAPABILITY}: a client cannot validate an identity token without it"
            )))
        }
        None => {}
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
            Error::Config(format!(
                "invalid {ENV_BIND} {bind_raw:?}; expected an address like \"127.0.0.1:8080\""
            ))
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

        let wait = match get(env, ENV_STORE_WAIT) {
            Some(raw) => match raw.parse::<u64>() {
                Ok(held) if (1..=MOST_STORE_WAIT_MILLIS).contains(&held) => {
                    std::time::Duration::from_millis(held)
                }
                _ => {
                    return Err(Error::Config(format!(
                        "invalid {ENV_STORE_WAIT} {raw:?}; expected 1 to {MOST_STORE_WAIT_MILLIS}"
                    )))
                }
            },
            None => DEFAULT_STORE_WAIT,
        };

        let versioning = match get(env, ENV_VERSIONING) {
            Some(raw) => fhir_api::Versioning::parse(raw, version)
                .map_err(|error| Error::Config(format!("invalid {ENV_VERSIONING}: {error}")))?,
            None => fhir_api::Versioning::default(),
        };

        let profiles = match get(env, ENV_PROFILE_VALIDATION) {
            Some(raw) => fhir_api::OnWrite::parse(raw).map_err(|error| {
                Error::Config(format!("invalid {ENV_PROFILE_VALIDATION}: {error}"))
            })?,
            None => fhir_api::OnWrite::default(),
        };

        let roles = match get(env, ENV_ROLES) {
            Some(raw) => fhir_api::Roles::parse(raw, version)
                .map_err(|error| Error::Config(format!("invalid {ENV_ROLES}: {error}")))?,
            None => fhir_api::Roles::default(),
        };

        let throttle = match get(env, ENV_AT_ONCE) {
            None => fhir_api::Throttle::unbounded(),
            Some(raw) => match raw.parse::<usize>() {
                Ok(held) if (1..=MOST_AT_ONCE).contains(&held) => fhir_api::Throttle::of(held)?,
                _ => {
                    return Err(Error::Config(format!(
                        "invalid {ENV_AT_ONCE} {raw:?}; expected 1 to {MOST_AT_ONCE}"
                    )))
                }
            },
        };

        let retention = match get(env, ENV_RETENTION) {
            Some(raw) => fhir_jobs::Retention::parse(raw, version)
                .map_err(|error| Error::Config(format!("invalid {ENV_RETENTION}: {error}")))?,
            None => fhir_jobs::Retention::default(),
        };

        let security_headers = match get(env, ENV_SECURITY_HEADERS) {
            None => fhir_api::SecurityHeaders::default(),
            Some(raw) => fhir_api::SecurityHeaders::parse(raw)
                .map_err(|error| Error::Config(format!("{ENV_SECURITY_HEADERS}: {error}")))?,
        };

        let restricted =
            fhir_api::Restricted::parse(listed(env, ENV_TYPES), listed(env, ENV_PARAMETERS))
                .map_err(|error| Error::Config(format!("{ENV_TYPES}: {error}")))?;

        let forwarding = match get(env, ENV_TRUST_PROXY) {
            None => fhir_api::Forwarding::untrusted(),
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" => fhir_api::Forwarding::trusted(),
                "false" | "no" | "off" => fhir_api::Forwarding::untrusted(),
                other => {
                    return Err(Error::Config(format!(
                        "{ENV_TRUST_PROXY} {other:?} is neither on nor off"
                    )))
                }
            },
        };

        let references = match get(env, ENV_REFERENCES) {
            None => fhir_api::References::as_written(),
            Some(raw) => {
                let (mode, also) = match raw.split_once(':') {
                    None => (raw.trim(), ""),
                    Some((mode, also)) => (mode.trim(), also),
                };
                let also: Vec<&str> = also.split(',').collect();
                match mode.to_ascii_lowercase().as_str() {
                    "as-written" => fhir_api::References::as_written(),
                    "absolute" => fhir_api::References::normalised(also),
                    "relative" => fhir_api::References::relative_both_ways(also),
                    other => {
                        return Err(Error::Config(format!(
                            "{ENV_REFERENCES} {other:?} is not \"as-written\", \"absolute\" or \
                             \"relative\""
                        )))
                    }
                }
            }
        };

        let number = |key: &str| -> Result<Option<usize>, Error> {
            match get(env, key) {
                None => Ok(None),
                Some(raw) => raw
                    .trim()
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|_| Error::Config(format!("{key} {raw:?} is not a number"))),
            }
        };
        let paging = fhir_api::Paging::new(
            number(ENV_PAGE_SIZE)?.unwrap_or(fhir_api::paging::DEFAULT_SIZE),
            number(ENV_PAGE_LIMIT)?.unwrap_or(fhir_api::paging::DEFAULT_LIMIT),
        )
        .map_err(|error| Error::Config(format!("{ENV_PAGE_SIZE}/{ENV_PAGE_LIMIT}: {error}")))?
        .sorting_by(get(env, ENV_DEFAULT_SORT).map(str::to_owned))
        .counting(match get(env, ENV_DEFAULT_TOTAL) {
            None => fhir_api::Counting::default(),
            Some(raw) => fhir_api::Counting::parse(raw)
                .map_err(|error| Error::Config(format!("{ENV_DEFAULT_TOTAL}: {error}")))?,
        });

        let body = match get(env, ENV_MAX_BODY) {
            None => None,
            Some(raw) => Some(
                fhir_api::limits::size(raw)
                    .map_err(|error| Error::Config(format!("{ENV_MAX_BODY}: {error}")))?,
            ),
        };
        let limits = fhir_api::Limits::new(body, number(ENV_MAX_ENTRIES)?)
            .map_err(|error| Error::Config(format!("{ENV_MAX_BODY}/{ENV_MAX_ENTRIES}: {error}")))?;

        let unchanged = match get(env, ENV_UNCHANGED) {
            None => fhir_api::Unchanged::silent(),
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "silent" => fhir_api::Unchanged::silent(),
                "changed" => fhir_api::Unchanged::parse("")
                    .map_err(|error| Error::Config(format!("{ENV_UNCHANGED}: {error}")))?,
                other if other.starts_with("changed:") => {
                    fhir_api::Unchanged::parse(&other["changed:".len()..])
                        .map_err(|error| Error::Config(format!("{ENV_UNCHANGED}: {error}")))?
                }
                other => {
                    return Err(Error::Config(format!(
                        "{ENV_UNCHANGED} {other:?} is not \"silent\", \"changed\" or \
                         \"changed:<meta elements>\""
                    )))
                }
            },
        };

        let default_format = match get(env, ENV_DEFAULT_FORMAT) {
            None => fhir_api::MediaType::DEFAULT,
            Some(raw) => fhir_api::MediaType::parse(raw.trim()).ok_or_else(|| {
                Error::Config(format!(
                    "{ENV_DEFAULT_FORMAT} {raw:?} names no representation this service answers"
                ))
            })?,
        };

        let preload = match get(env, ENV_PRELOAD) {
            None => None,
            Some(raw) => {
                let held = std::path::PathBuf::from(raw.trim());
                if !held.is_dir() {
                    return Err(Error::Config(format!(
                        "{ENV_PRELOAD} {raw:?} is not a directory"
                    )));
                }
                Some(held)
            }
        };

        let reset = match get(env, ENV_RESET) {
            None => fhir_api::Resettable::never(),
            Some(raw) => fhir_api::Resettable::named(raw)
                .map_err(|error| Error::Config(format!("{ENV_RESET}: {error}")))?,
        };

        let collector = match get(env, ENV_OTLP) {
            None => None,
            Some(raw) => Some(
                crate::otlp::Collector::parse(raw)
                    .map_err(|error| Error::Config(format!("{ENV_OTLP}: {error}")))?,
            ),
        };

        let alert = match get(env, ENV_ALERT) {
            None => None,
            Some(raw) => Some(
                crate::alarm::Called::parse(raw)
                    .map_err(|error| Error::Config(format!("{ENV_ALERT}: {error}")))?,
            ),
        };

        let policies = match get(env, ENV_ACCESS_POLICY) {
            None => fhir_api::Policies::off(),
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" => fhir_api::Policies::on()
                    .map_err(|error| Error::Config(format!("{ENV_ACCESS_POLICY}: {error}")))?,
                "false" | "no" | "off" => fhir_api::Policies::off(),
                other => {
                    return Err(Error::Config(format!(
                        "{ENV_ACCESS_POLICY} {other:?} is neither on nor off"
                    )))
                }
            },
        };

        let tenancy = match get(env, ENV_TENANCY) {
            None => fhir_api::Tenancy::off(),
            Some(system) => {
                let claim = get(env, ENV_TENANT_CLAIM).unwrap_or("tenant");
                fhir_api::Tenancy::by_label(system, claim)
                    .map_err(|error| Error::Config(format!("{ENV_TENANCY}: {error}")))?
            }
        };

        let networks = listed(env, ENV_ADMINISTRATION);
        let administration = match networks.is_empty() {
            true => fhir_api::Administration::off(),
            false => fhir_api::Administration::restricted_to(networks)
                .map_err(|error| Error::Config(format!("{ENV_ADMINISTRATION}: {error}")))?,
        };

        let mut versions = vec![version];
        for raw in listed(env, ENV_VERSIONS) {
            let held = raw.parse::<FhirVersion>().map_err(|_| {
                Error::Config(format!(
                    "invalid {ENV_VERSIONS} {raw:?}; expected STU3, R4, R4B or R5"
                ))
            })?;
            if !versions.contains(&held) {
                versions.push(held);
            }
        }

        let capabilities = fhir_api::Capabilities {
            conditional_delete: match get(env, ENV_CONDITIONAL_DELETE) {
                Some(raw) => fhir_api::ConditionalDelete::parse(raw).map_err(|error| {
                    Error::Config(format!("invalid {ENV_CONDITIONAL_DELETE}: {error}"))
                })?,
                None => fhir_api::ConditionalDelete::default(),
            },
            create_on_update: match get(env, ENV_CREATE_ON_UPDATE) {
                None => true,
                Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" => true,
                    "false" | "no" | "off" => false,
                    other => {
                        return Err(Error::Config(format!(
                            "invalid {ENV_CREATE_ON_UPDATE} {other:?}; expected true or false"
                        )))
                    }
                },
            },
            include_depth: match get(env, ENV_INCLUDE_DEPTH) {
                None => fhir_store::DEFAULT_INCLUDE_DEPTH,
                Some(raw) => raw
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|held| (1..=MOST_INCLUDE_DEPTH).contains(held))
                    .ok_or_else(|| {
                        Error::Config(format!(
                            "invalid {ENV_INCLUDE_DEPTH} {raw:?}; expected 1 to {MOST_INCLUDE_DEPTH}"
                        ))
                    })?,
            },
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
            wait,
            authorization: authorization(env)?,
            keys: keys(env)?,
            pins: listed(env, ENV_ISSUER_PINS),
            scrape: scrape(env)?,
            versioning,
            profiles,
            roles,
            throttle,
            retention,
            capabilities,
            allowed_profiles: fhir_api::AllowedProfiles::new(listed(env, ENV_ALLOWED_PROFILES)),
            versions,
            administration,
            tenancy,
            policies,
            security_headers,
            alert,
            collector,
            preload,
            reset,
            unchanged,
            default_format,
            paging,
            limits,
            forwarding,
            references,
            restricted,
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
    env.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
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
        env.insert(
            ENV_AUTH_ISSUER.to_owned(),
            "https://issuer.example.org".to_owned(),
        );
        let error = Config::parse(&env).expect_err("a half-set authorization must fail");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains(ENV_AUTH_AUTHORIZE));
        env.insert(
            ENV_AUTH_AUTHORIZE.to_owned(),
            "https://issuer.example.org/a".to_owned(),
        );
        let error = Config::parse(&env).expect_err("a missing token endpoint must fail");
        assert!(error.to_string().contains(ENV_AUTH_TOKEN));
    }

    #[test]
    fn an_issuer_without_an_audience_fails_fast() {
        let mut env = env_empty();
        env.insert(
            ENV_AUTH_ISSUER.to_owned(),
            "https://issuer.example.org".to_owned(),
        );
        env.insert(
            ENV_AUTH_AUTHORIZE.to_owned(),
            "https://issuer.example.org/a".to_owned(),
        );
        env.insert(
            ENV_AUTH_TOKEN.to_owned(),
            "https://issuer.example.org/t".to_owned(),
        );
        let error = Config::parse(&env).expect_err("an audience is required");
        assert!(
            format!("{error:?}").contains(ENV_AUTH_AUDIENCE),
            "{error:?}"
        );
    }

    #[test]
    fn the_configured_authorization_is_the_one_reported() {
        let mut env = env_empty();
        env.insert(
            ENV_AUTH_ISSUER.to_owned(),
            "https://issuer.example.org".to_owned(),
        );
        env.insert(
            ENV_AUTH_AUTHORIZE.to_owned(),
            "https://issuer.example.org/a".to_owned(),
        );
        env.insert(
            ENV_AUTH_TOKEN.to_owned(),
            "https://issuer.example.org/t".to_owned(),
        );
        env.insert(
            ENV_AUTH_AUDIENCE.to_owned(),
            "https://service.example.org".to_owned(),
        );
        env.insert(
            ENV_AUTH_INTROSPECT.to_owned(),
            "https://issuer.example.org/i".to_owned(),
        );
        env.insert(
            ENV_AUTH_SCOPES.to_owned(),
            "system/*.read, system/*.write".to_owned(),
        );
        env.insert(
            ENV_AUTH_CAPABILITIES.to_owned(),
            "client-confidential-symmetric".to_owned(),
        );
        let config = Config::parse(&env).expect("a complete authorization must parse");
        let active = config
            .authorization
            .clone()
            .expect("authorization must be active");
        assert_eq!(active.issuer, "https://issuer.example.org");
        assert_eq!(
            active.introspect.as_deref(),
            Some("https://issuer.example.org/i")
        );
        assert_eq!(
            active.scopes,
            vec!["system/*.read".to_owned(), "system/*.write".to_owned()]
        );
        assert_eq!(
            active.capabilities,
            vec!["client-confidential-symmetric".to_owned()]
        );
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
        assert_eq!(
            Config::parse(&env)
                .expect("one connection parses")
                .entries(),
            1
        );
    }

    #[test]
    fn the_connection_wait_is_bounded_and_configurable() {
        let mut env = env_empty();
        let config = Config::parse(&env).expect("defaults must parse");
        assert_eq!(config.wait, DEFAULT_STORE_WAIT);
        assert!(config.wait <= std::time::Duration::from_secs(5));

        env.insert(ENV_STORE_WAIT.to_owned(), "250".to_owned());
        let brief = Config::parse(&env).expect("a named wait must parse");
        assert_eq!(brief.wait, std::time::Duration::from_millis(250));
    }

    #[test]
    fn the_thumbprints_discovery_may_return_are_configured() {
        let mut env = env_empty();
        assert!(Config::parse(&env)
            .expect("defaults must parse")
            .pins
            .is_empty());
        env.insert(ENV_ISSUER_PINS.to_owned(), "one, two".to_owned());
        let pinned = Config::parse(&env).expect("named pins must parse");
        assert_eq!(pinned.pins, vec!["one".to_owned(), "two".to_owned()]);
    }

    #[test]
    fn a_wait_outside_the_range_fails_fast() {
        let mut env = env_empty();
        for raw in ["0", "none", "-1", "600000"] {
            env.insert(ENV_STORE_WAIT.to_owned(), raw.to_owned());
            let error = Config::parse(&env).expect_err("an unusable wait must fail");
            assert!(error.to_string().contains(ENV_STORE_WAIT), "{raw}");
        }
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
        env.insert(
            ENV_DOCUMENT_URL.to_owned(),
            "mongodb://127.0.0.1:27018".to_owned(),
        );
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
    fn a_versioning_policy_is_read_and_validated_at_startup() {
        let mut env = env_empty();
        env.insert(
            ENV_VERSIONING.to_owned(),
            "versioned;Patient=versioned-update;AuditEvent=no-version".to_owned(),
        );
        let config = Config::parse(&env).expect("the setting must parse");
        let kind = |name: &str| name.parse::<fhir_core::ResourceType>().unwrap();
        assert_eq!(
            config.versioning.of(kind("Patient")),
            fhir_api::Policy::VersionedUpdate
        );
        assert_eq!(
            config.versioning.of(kind("AuditEvent")),
            fhir_api::Policy::NoVersion
        );
        assert_eq!(
            config.versioning.of(kind("Observation")),
            fhir_api::Policy::Versioned
        );
    }

    #[test]
    fn nothing_configured_leaves_every_type_versioned() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(config.versioning.is_default());
    }

    #[test]
    fn an_unknown_versioning_policy_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_VERSIONING.to_owned(), "Patient=sometimes".to_owned());
        let error = Config::parse(&env).expect_err("an unknown policy must fail");
        assert!(error.to_string().contains(ENV_VERSIONING), "{error}");
        assert!(error.to_string().contains("sometimes"), "{error}");
    }

    #[test]
    fn a_versioning_policy_for_a_type_the_release_does_not_serve_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_VERSION.to_owned(), "STU3".to_owned());
        env.insert(ENV_VERSIONING.to_owned(), "Citation=versioned".to_owned());
        let error = Config::parse(&env).expect_err("an unserved type must fail");
        assert!(error.to_string().contains("does not serve"), "{error}");
    }

    #[test]
    fn the_browser_headers_are_sent_unless_an_operator_says_otherwise() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(!config.security_headers.is_empty());
        let mut env = env_empty();
        env.insert(ENV_SECURITY_HEADERS.to_owned(), "all=off".to_owned());
        assert!(Config::parse(&env)
            .expect("all=off parses")
            .security_headers
            .is_empty());
        env.insert(ENV_SECURITY_HEADERS.to_owned(), "nonsense=1".to_owned());
        let error = Config::parse(&env).expect_err("an unknown header must fail");
        assert!(error.to_string().contains(ENV_SECURITY_HEADERS), "{error}");
    }

    #[test]
    fn a_proxy_is_trusted_only_where_an_operator_says_so() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(!config.forwarding.is_trusted());
        let mut env = env_empty();
        env.insert(ENV_TRUST_PROXY.to_owned(), "on".to_owned());
        assert!(Config::parse(&env)
            .expect("on parses")
            .forwarding
            .is_trusted());
        env.insert(ENV_TRUST_PROXY.to_owned(), "sometimes".to_owned());
        let error = Config::parse(&env).expect_err("a value that is neither must fail");
        assert!(error.to_string().contains(ENV_TRUST_PROXY), "{error}");
    }

    #[test]
    fn how_references_are_stored_is_read_and_validated() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(!config.references.is_on(), "stored as written by default");
        let mut env = env_empty();
        env.insert(ENV_REFERENCES.to_owned(), "absolute:url".to_owned());
        let config = Config::parse(&env).expect("absolute parses");
        assert!(config.references.is_on());
        assert!(config.references.answers_absolute());
        env.insert(ENV_REFERENCES.to_owned(), "relative".to_owned());
        let config = Config::parse(&env).expect("relative parses");
        assert!(config.references.is_on());
        assert!(!config.references.answers_absolute());
        env.insert(ENV_REFERENCES.to_owned(), "whatever".to_owned());
        let error = Config::parse(&env).expect_err("an unknown mode must fail");
        assert!(error.to_string().contains(ENV_REFERENCES), "{error}");
    }

    #[test]
    fn the_page_bounds_are_read_and_validated_at_startup() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert_eq!(config.paging.size(), fhir_api::paging::DEFAULT_SIZE);
        assert_eq!(config.paging.limit(), fhir_api::paging::DEFAULT_LIMIT);
        assert_eq!(
            config.paging.total(),
            fhir_api::Counting::Accurate,
            "an instance that says nothing counts, as this build always has"
        );
        let mut env = env_empty();
        env.insert(ENV_PAGE_SIZE.to_owned(), "5".to_owned());
        env.insert(ENV_PAGE_LIMIT.to_owned(), "50".to_owned());
        env.insert(ENV_DEFAULT_SORT.to_owned(), "-_lastUpdated".to_owned());
        env.insert(ENV_DEFAULT_TOTAL.to_owned(), "none".to_owned());
        let config = Config::parse(&env).expect("the settings parse");
        assert_eq!(config.paging.size(), 5);
        assert_eq!(config.paging.sort(), Some("-_lastUpdated"));
        assert_eq!(config.paging.total(), fhir_api::Counting::None);

        env.insert(ENV_PAGE_SIZE.to_owned(), "500".to_owned());
        let error = Config::parse(&env).expect_err("a default past the bound must fail");
        assert!(error.to_string().contains(ENV_PAGE_SIZE), "{error}");
    }

    #[test]
    fn the_input_bounds_are_read_and_validated_at_startup() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert_eq!(config.limits.body(), None, "unbounded, as it always was");
        let mut env = env_empty();
        env.insert(ENV_MAX_BODY.to_owned(), "1MiB".to_owned());
        env.insert(ENV_MAX_ENTRIES.to_owned(), "150".to_owned());
        let config = Config::parse(&env).expect("the settings parse");
        assert_eq!(config.limits.body(), Some(1_048_576));
        assert_eq!(config.limits.entries(), Some(150));
        env.insert(ENV_MAX_BODY.to_owned(), "one gigabyte".to_owned());
        let error = Config::parse(&env).expect_err("a size that is no size must fail");
        assert!(error.to_string().contains(ENV_MAX_BODY), "{error}");
    }

    #[test]
    fn a_write_that_wrote_nothing_says_so_only_where_asked() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(!config.unchanged.is_on());
        let mut env = env_empty();
        env.insert(ENV_UNCHANGED.to_owned(), "changed".to_owned());
        let config = Config::parse(&env).expect("changed parses");
        assert!(config.unchanged.is_on());
        assert!(config.unchanged.ignoring().is_empty());
        env.insert(ENV_UNCHANGED.to_owned(), "changed:tag,security".to_owned());
        let config = Config::parse(&env).expect("a list parses");
        assert_eq!(config.unchanged.ignoring(), ["tag", "security"]);
        env.insert(ENV_UNCHANGED.to_owned(), "changed:id".to_owned());
        let error = Config::parse(&env).expect_err("only meta elements may be ignored");
        assert!(error.to_string().contains(ENV_UNCHANGED), "{error}");
    }

    #[test]
    fn the_default_representation_is_read_and_validated() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert_eq!(config.default_format, fhir_api::MediaType::DEFAULT);
        let mut env = env_empty();
        env.insert(
            ENV_DEFAULT_FORMAT.to_owned(),
            "application/fhir+xml".to_owned(),
        );
        assert_eq!(
            Config::parse(&env)
                .expect("a media type parses")
                .default_format,
            fhir_api::MediaType::FhirXml
        );
        env.insert(ENV_DEFAULT_FORMAT.to_owned(), "text/csv".to_owned());
        let error = Config::parse(&env).expect_err("an unserved representation must fail");
        assert!(error.to_string().contains(ENV_DEFAULT_FORMAT), "{error}");
    }

    #[test]
    fn an_instance_may_not_be_emptied_unless_it_is_named() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(!config.reset.is_allowed());
        let mut env = env_empty();
        env.insert(ENV_RESET.to_owned(), "staging".to_owned());
        let config = Config::parse(&env).expect("a name parses");
        assert!(config.reset.is_allowed());
        assert_eq!(config.reset.name(), Some("staging"));
        env.insert(ENV_RESET.to_owned(), "   ".to_owned());
        assert!(
            !Config::parse(&env)
                .expect("a blank setting parses")
                .reset
                .is_allowed(),
            "a setting with nothing in it is a setting that was not made"
        );
    }

    #[test]
    fn a_preload_directory_that_is_not_there_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_PRELOAD.to_owned(), "/nowhere/at/all".to_owned());
        let error = Config::parse(&env).expect_err("a missing directory must fail");
        assert!(error.to_string().contains(ENV_PRELOAD), "{error}");
    }

    #[test]
    fn a_collector_address_is_read_and_validated_at_startup() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(config.collector.is_none(), "nothing is exported by default");
        let mut env = env_empty();
        env.insert(ENV_OTLP.to_owned(), "127.0.0.1:4318".to_owned());
        assert!(Config::parse(&env)
            .expect("an address parses")
            .collector
            .is_some());
        env.insert(ENV_OTLP.to_owned(), "collector".to_owned());
        let error = Config::parse(&env).expect_err("a name that is no address must fail");
        assert!(error.to_string().contains(ENV_OTLP), "{error}");
    }

    #[test]
    fn an_alert_address_is_read_and_validated_at_startup() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(config.alert.is_none(), "nobody is called by default");
        let mut env = env_empty();
        env.insert(ENV_ALERT.to_owned(), "127.0.0.1:9000/alerts".to_owned());
        assert!(Config::parse(&env)
            .expect("an address parses")
            .alert
            .is_some());
        env.insert(ENV_ALERT.to_owned(), "nowhere".to_owned());
        let error = Config::parse(&env).expect_err("a name that is no address must fail");
        assert!(error.to_string().contains(ENV_ALERT), "{error}");
    }

    #[test]
    fn policies_are_consulted_only_where_they_are_asked_for() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(!config.policies.is_on());
        let mut env = env_empty();
        env.insert(ENV_ACCESS_POLICY.to_owned(), "on".to_owned());
        assert!(Config::parse(&env).expect("on parses").policies.is_on());
        env.insert(ENV_ACCESS_POLICY.to_owned(), "perhaps".to_owned());
        let error = Config::parse(&env).expect_err("a value that is neither must fail");
        assert!(error.to_string().contains(ENV_ACCESS_POLICY), "{error}");
    }

    #[test]
    fn one_door_unless_networks_are_named() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(!config.administration.is_on());
    }

    #[test]
    fn the_networks_named_open_the_other_door() {
        let mut env = env_empty();
        env.insert(ENV_ADMINISTRATION.to_owned(), "127.0.0.0/8, ::1".to_owned());
        let config = Config::parse(&env).expect("two networks must parse");
        assert!(config.administration.is_on());
        assert!(config.administration.admits("127.0.0.5".parse().unwrap()));
        assert!(!config.administration.admits("9.9.9.9".parse().unwrap()));
    }

    #[test]
    fn what_is_not_a_network_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_ADMINISTRATION.to_owned(), "everywhere".to_owned());
        let error = Config::parse(&env).expect_err("a name that is no network must fail");
        assert!(error.to_string().contains(ENV_ADMINISTRATION), "{error}");
    }

    #[test]
    fn one_release_is_served_unless_more_are_named() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert_eq!(config.versions, vec![FhirVersion::R4]);
    }

    #[test]
    fn the_releases_named_are_served_beside_the_default() {
        let mut env = env_empty();
        env.insert(ENV_VERSION.to_owned(), "R4".to_owned());
        env.insert(ENV_VERSIONS.to_owned(), "R5, STU3, R4".to_owned());
        let config = Config::parse(&env).expect("the setting must parse");
        assert_eq!(
            config.versions,
            vec![FhirVersion::R4, FhirVersion::R5, FhirVersion::Stu3],
            "the default is first and is not repeated"
        );
    }

    #[test]
    fn a_release_that_is_no_release_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_VERSIONS.to_owned(), "R6".to_owned());
        let error = Config::parse(&env).expect_err("an unknown release must fail");
        assert!(error.to_string().contains(ENV_VERSIONS), "{error}");
    }

    #[test]
    fn a_retention_rule_is_read_and_validated_at_startup() {
        let mut env = env_empty();
        env.insert(
            ENV_RETENTION.to_owned(),
            "AuditEvent=90;Observation=3650:purge".to_owned(),
        );
        let config = Config::parse(&env).expect("the setting must parse");
        assert_eq!(config.retention.rules().len(), 2);
        assert_eq!(config.retention.rules()[1].days, 3650);
    }

    #[test]
    fn an_unknown_retention_window_fails_fast() {
        let mut env = env_empty();
        env.insert(ENV_RETENTION.to_owned(), "AuditEvent=never".to_owned());
        let error = Config::parse(&env).expect_err("an unknown window must fail");
        assert!(error.to_string().contains(ENV_RETENTION), "{error}");
    }

    #[test]
    fn nothing_configured_retains_everything() {
        let config = Config::parse(&env_empty()).expect("an empty environment must parse");
        assert!(config.retention.is_empty());
        assert!(!config.throttle.is_bounded());
    }

    #[test]
    fn a_load_bound_is_read_and_validated_at_startup() {
        let mut env = env_empty();
        env.insert(ENV_AT_ONCE.to_owned(), "32".to_owned());
        let config = Config::parse(&env).expect("the bound must parse");
        assert!(config.throttle.is_bounded());
        assert_eq!(config.throttle.at_once(), 32);
    }

    #[test]
    fn a_load_bound_outside_its_range_fails_fast() {
        for raw in ["0", "-1", "many"] {
            let mut env = env_empty();
            env.insert(ENV_AT_ONCE.to_owned(), raw.to_owned());
            let error = Config::parse(&env).expect_err("an invalid bound must fail");
            assert!(error.to_string().contains(ENV_AT_ONCE), "{raw}: {error}");
        }
    }

    #[test]
    fn roles_are_read_and_validated_at_startup() {
        let mut env = env_empty();
        env.insert(ENV_ROLES.to_owned(), "clinician=read,write".to_owned());
        let config = Config::parse(&env).expect("the roles must parse");
        assert!(!config.roles.is_empty());
        let mut bad = env_empty();
        bad.insert(ENV_ROLES.to_owned(), "clinician=peek".to_owned());
        let error = Config::parse(&bad).expect_err("an unknown action must fail");
        assert!(error.to_string().contains(ENV_ROLES), "{error}");
    }

    #[test]
    fn profile_validation_is_read_and_validated_at_startup() {
        let mut env = env_empty();
        env.insert(
            ENV_PROFILE_VALIDATION.to_owned(),
            "create,update".to_owned(),
        );
        let config = Config::parse(&env).expect("the setting must parse");
        assert!(config.profiles.create && config.profiles.update);
        let mut bad = env_empty();
        bad.insert(ENV_PROFILE_VALIDATION.to_owned(), "always".to_owned());
        let error = Config::parse(&bad).expect_err("an unknown mode must fail");
        assert!(
            error.to_string().contains(ENV_PROFILE_VALIDATION),
            "{error}"
        );
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
            Config::parse(&env)
                .expect("a default address is enough")
                .document_url,
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
        assert_eq!(
            held.systems().len(),
            fhir_core::Catalogue::of(config.version).systems().len()
        );
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
