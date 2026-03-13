use fhir_core::{Error, FhirVersion, ResourceType};
use std::collections::HashMap;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    NoVersion,

    #[default]
    Versioned,

    VersionedUpdate,
}

impl Policy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Policy::NoVersion => "no-version",
            Policy::Versioned => "versioned",
            Policy::VersionedUpdate => "versioned-update",
        }
    }

    pub fn keeps_history(&self) -> bool {
        !matches!(self, Policy::NoVersion)
    }

    pub fn needs_match(&self) -> bool {
        matches!(self, Policy::VersionedUpdate)
    }
}

impl FromStr for Policy {
    type Err = Error;

    fn from_str(text: &str) -> Result<Policy, Error> {
        match text.trim() {
            "no-version" => Ok(Policy::NoVersion),
            "versioned" => Ok(Policy::Versioned),
            "versioned-update" => Ok(Policy::VersionedUpdate),
            other => Err(Error::Config(format!(
                "versioning policy {other:?} names none of no-version, versioned, versioned-update"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Versioning {
    fallback: Policy,
    named: HashMap<ResourceType, Policy>,
}

impl Versioning {
    pub fn parse(raw: &str, version: FhirVersion) -> Result<Versioning, Error> {
        let mut held = Versioning::default();
        for part in raw
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            match part.split_once('=') {
                None => held.fallback = part.parse::<Policy>()?,
                Some((name, policy)) => {
                    let kind = name.trim().parse::<ResourceType>().map_err(|_| {
                        Error::Config(format!(
                            "versioning names {:?}, which is no resource type",
                            name.trim()
                        ))
                    })?;
                    if !ResourceType::served(version).contains(&kind) {
                        return Err(Error::Config(format!(
                            "versioning names {kind}, which {version} does not serve"
                        )));
                    }
                    held.named.insert(kind, policy.parse::<Policy>()?);
                }
            }
        }
        Ok(held)
    }

    pub fn of(&self, kind: ResourceType) -> Policy {
        self.named.get(&kind).copied().unwrap_or(self.fallback)
    }

    pub fn is_default(&self) -> bool {
        self.named.is_empty() && self.fallback == Policy::Versioned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a served type")
    }

    #[test]
    fn nothing_configured_keeps_every_version_of_every_type() {
        let held = Versioning::default();
        assert_eq!(held.of(kind("Patient")), Policy::Versioned);
        assert!(held.is_default());
    }

    #[test]
    fn a_default_and_an_override_are_read() {
        let held = Versioning::parse(
            "versioned;Patient=versioned-update;AuditEvent=no-version",
            FhirVersion::R4,
        )
        .unwrap();
        assert_eq!(held.of(kind("Patient")), Policy::VersionedUpdate);
        assert_eq!(held.of(kind("AuditEvent")), Policy::NoVersion);
        assert_eq!(held.of(kind("Observation")), Policy::Versioned);
        assert!(!held.is_default());
    }

    #[test]
    fn a_bare_policy_sets_the_default_for_every_type() {
        let held = Versioning::parse("no-version", FhirVersion::R4).unwrap();
        assert_eq!(held.of(kind("Observation")), Policy::NoVersion);
    }

    #[test]
    fn an_unknown_policy_is_refused() {
        let error = Versioning::parse("Patient=sometimes", FhirVersion::R4).unwrap_err();
        assert!(error.to_string().contains("sometimes"), "{error}");
    }

    #[test]
    fn an_unknown_type_is_refused() {
        let error = Versioning::parse("Nonesuch=versioned", FhirVersion::R4).unwrap_err();
        assert!(error.to_string().contains("Nonesuch"), "{error}");
    }

    #[test]
    fn a_type_the_release_does_not_serve_is_refused() {
        let error = Versioning::parse("Citation=versioned", FhirVersion::Stu3).unwrap_err();
        assert!(error.to_string().contains("does not serve"), "{error}");
    }
}
