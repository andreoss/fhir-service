use crate::search::compartment::Compartment;
use crate::{Error, ResourceId, ResourceType};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Grant {
    pub types: Vec<ResourceType>,
    pub compartments: Vec<Compartment>,
}

impl Grant {
    pub fn parse(raw: &str) -> Result<Grant, Error> {
        let mut grant = Grant::default();
        for entry in raw.split(';').map(str::trim).filter(|part| !part.is_empty()) {
            let (key, value) = entry
                .split_once('=')
                .ok_or_else(|| Error::InvalidParameter(format!("grant entry {entry:?}")))?;
            match key.trim() {
                "types" => grant.types.extend(parsed_types(value)?),
                "compartment" => grant.compartments.push(compartment(value)?),
                other => {
                    return Err(Error::UnsupportedParameter(format!("grant key {other:?}")))
                }
            }
        }
        Ok(grant)
    }

    pub fn admits(&self, resource_type: ResourceType) -> bool {
        self.types.is_empty() || self.types.contains(&resource_type)
    }

    pub fn is_open(&self) -> bool {
        self.compartments.is_empty()
    }
}

fn parsed_types(raw: &str) -> Result<Vec<ResourceType>, Error> {
    raw.split(',')
        .filter(|part| !part.is_empty())
        .map(str::parse::<ResourceType>)
        .collect()
}

fn compartment(raw: &str) -> Result<Compartment, Error> {
    let (kind, id) = raw
        .split_once('/')
        .ok_or_else(|| Error::InvalidParameter(format!("grant compartment {raw:?}")))?;
    Ok(Compartment {
        kind: kind.parse::<ResourceType>()?,
        id: ResourceId::parse(id)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_carries_types_and_compartments() {
        let grant = Grant::parse("types=Patient,Observation;compartment=Patient/pt-1").unwrap();
        assert!(grant.admits("Patient".parse().unwrap()));
        assert!(!grant.admits("Encounter".parse().unwrap()));
        assert!(!grant.is_open());
        assert_eq!(grant.compartments[0].id.as_str(), "pt-1");
    }

    #[test]
    fn an_empty_grant_admits_everything() {
        let grant = Grant::parse("").unwrap();
        assert!(grant.admits("Encounter".parse().unwrap()));
        assert!(grant.is_open());
        assert_eq!(grant, Grant::default());
    }

    #[test]
    fn a_malformed_grant_is_rejected() {
        assert!(matches!(
            Grant::parse("types").unwrap_err(),
            Error::InvalidParameter(_)
        ));
        assert!(matches!(
            Grant::parse("compartment=Patient").unwrap_err(),
            Error::InvalidParameter(_)
        ));
        assert!(matches!(
            Grant::parse("scope=all").unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        assert!(Grant::parse("types=Nonesuch").is_err());
        assert!(Grant::parse("compartment=Patient/not a id").is_err());
    }
}
