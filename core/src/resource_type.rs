use crate::model::Model;
use crate::{Error, FhirVersion};
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

fn union() -> &'static [String] {
    static HELD: OnceLock<Vec<String>> = OnceLock::new();
    HELD.get_or_init(|| {
        let mut names: Vec<String> = FhirVersion::ALL
            .into_iter()
            .flat_map(|version| Model::of(version).resources().map(str::to_owned).collect::<Vec<_>>())
            .collect::<std::collections::BTreeSet<String>>()
            .into_iter()
            .collect();
        names.sort();
        names
    })
}

pub fn resource_types() -> &'static [String] {
    union()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceType(&'static str);

impl ResourceType {
    pub fn as_str(&self) -> &str {
        self.0
    }

    pub fn all() -> Vec<ResourceType> {
        union().iter().map(|name| ResourceType(name)).collect()
    }

    pub fn served(version: FhirVersion) -> Vec<ResourceType> {
        let model = Model::of(version);
        union()
            .iter()
            .filter(|name| model.has_resource(name))
            .map(|name| ResourceType(name))
            .collect()
    }

    pub fn served_by(&self, version: FhirVersion) -> bool {
        Model::of(version).has_resource(self.0)
    }
}

impl FromStr for ResourceType {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        let trimmed = value.trim();
        match union().binary_search_by(|name| name.as_str().cmp(trimmed)) {
            Ok(index) => Ok(ResourceType(&union()[index])),
            Err(_) => Err(Error::InvalidResourceType(value.to_owned())),
        }
    }
}

impl TryFrom<&str> for ResourceType {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> {
        ResourceType::from_str(value)
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_resource_types() {
        for value in ["Patient", "Observation", "Encounter", "MedicationRequest"] {
            let rt: ResourceType = value.parse().unwrap();
            assert_eq!(rt.as_str(), value);
        }
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let rt: ResourceType = "  Patient ".parse().unwrap();
        assert_eq!(rt.as_str(), "Patient");
    }

    #[test]
    fn rejects_unknown_or_malformed_types() {
        for value in ["patient", "Patien", "Patient1", "Foo", ""] {
            assert!(matches!(value.parse::<ResourceType>(), Err(Error::InvalidResourceType(_))));
        }
    }

    #[test]
    fn display_round_trips() {
        let rt: ResourceType = "Condition".parse().unwrap();
        assert_eq!(rt.to_string(), "Condition");
    }
}