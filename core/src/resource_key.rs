use crate::{ResourceId, ResourceType};
use std::fmt;
use std::str::FromStr;









#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceKey {
    resource_type: ResourceType,
    id: ResourceId,
}

impl ResourceKey {
    pub fn new(resource_type: ResourceType, id: ResourceId) -> ResourceKey {
        ResourceKey { resource_type, id }
    }

    pub fn resource_type(&self) -> ResourceType {
        self.resource_type
    }

    pub fn id(&self) -> &ResourceId {
        &self.id
    }

    
    pub fn of(envelope: &crate::ResourceEnvelope) -> ResourceKey {
        ResourceKey::new(envelope.resource_type(), envelope.id().clone())
    }

    
    pub fn reference(&self) -> String {
        format!("{}/{}", self.resource_type.as_str(), self.id.as_str())
    }
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}/{}", self.resource_type.as_str(), self.id.as_str())
    }
}

impl FromStr for ResourceKey {
    type Err = crate::Error;

    
    fn from_str(text: &str) -> Result<ResourceKey, crate::Error> {
        let (kind, id) = text.split_once('/').ok_or_else(|| {
            crate::Error::InvalidResourceId(format!("{text:?} names no type and id"))
        })?;
        Ok(ResourceKey::new(
            kind.parse::<ResourceType>()?,
            ResourceId::parse(id)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a known type")
    }

    #[test]
    fn a_key_is_its_type_and_its_id() {
        let held = ResourceKey::new(kind("Patient"), ResourceId::parse("one").unwrap());
        assert_eq!(held.resource_type(), kind("Patient"));
        assert_eq!(held.id().as_str(), "one");
        assert_eq!(held.to_string(), "Patient/one");
        assert_eq!(held.reference(), "Patient/one");
    }

    #[test]
    fn the_same_id_under_two_types_names_two_resources() {
        let id = ResourceId::parse("shared").unwrap();
        let one = ResourceKey::new(kind("Encounter"), id.clone());
        let other = ResourceKey::new(kind("Specimen"), id);
        assert_ne!(one, other);
    }

    #[test]
    fn a_reference_reads_back_as_a_key() {
        let held: ResourceKey = "Observation/ob-1".parse().unwrap();
        assert_eq!(held.resource_type(), kind("Observation"));
        assert_eq!(held.id().as_str(), "ob-1");
    }

    #[test]
    fn something_that_is_no_reference_is_refused() {
        assert!("ob-1".parse::<ResourceKey>().is_err());
        assert!("Nonesuch/ob-1".parse::<ResourceKey>().is_err());
    }
}
