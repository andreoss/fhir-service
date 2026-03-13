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
            .flat_map(|version| {
                Model::of(version)
                    .resources()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
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

fn registered() -> &'static std::sync::RwLock<std::collections::BTreeSet<&'static str>> {
    static HELD: OnceLock<std::sync::RwLock<std::collections::BTreeSet<&'static str>>> =
        OnceLock::new();
    HELD.get_or_init(|| std::sync::RwLock::new(std::collections::BTreeSet::new()))
}

pub fn register(name: &str) -> Result<ResourceType, Error> {
    let held = name.trim();
    if let Ok(found) = held.parse::<ResourceType>() {
        return Ok(found);
    }
    let plausible = !held.is_empty()
        && held.len() <= 64
        && held.starts_with(|letter: char| letter.is_ascii_uppercase())
        && held.chars().all(|letter| letter.is_ascii_alphanumeric());
    if !plausible {
        return Err(Error::InvalidResourceType(name.to_owned()));
    }
    let leaked: &'static str = Box::leak(held.to_owned().into_boxed_str());
    let mut names = registered()
        .write()
        .map_err(|_| Error::Internal("the registry lock is poisoned".to_owned()))?;
    let found = *names.get(held).unwrap_or(&leaked);
    names.insert(found);
    Ok(ResourceType(found))
}

pub fn is_registered(name: &str) -> bool {
    registered()
        .read()
        .map(|held| held.contains(name))
        .unwrap_or(false)
}

fn custom() -> Vec<ResourceType> {
    registered()
        .read()
        .map(|held| held.iter().map(|name| ResourceType(name)).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
        let mut held: Vec<ResourceType> = union()
            .iter()
            .filter(|name| model.has_resource(name))
            .map(|name| ResourceType(name))
            .collect();
        held.extend(custom());
        held
    }

    pub fn served_by(&self, version: FhirVersion) -> bool {
        Model::of(version).has_resource(self.0) || is_registered(self.0)
    }

    pub fn is_custom(&self) -> bool {
        is_registered(self.0)
    }
}

impl FromStr for ResourceType {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        let trimmed = value.trim();
        match union().binary_search_by(|name| name.as_str().cmp(trimmed)) {
            Ok(index) => Ok(ResourceType(&union()[index])),
            Err(_) => match registered()
                .read()
                .ok()
                .and_then(|held| held.get(trimmed).copied())
            {
                Some(name) => Ok(ResourceType(name)),
                None => Err(Error::InvalidResourceType(value.to_owned())),
            },
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
            assert!(matches!(
                value.parse::<ResourceType>(),
                Err(Error::InvalidResourceType(_))
            ));
        }
    }

    #[test]
    fn display_round_trips() {
        let rt: ResourceType = "Condition".parse().unwrap();
        assert_eq!(rt.to_string(), "Condition");
    }
}

#[cfg(test)]
mod custom_tests {
    use super::*;

    #[test]
    fn a_name_no_publication_carries_is_refused_until_it_is_registered() {
        assert!("Widget".parse::<ResourceType>().is_err());
        let held = register("Widget").expect("a plausible name");
        assert_eq!(held.as_str(), "Widget");
        assert!(held.is_custom());
        assert_eq!("Widget".parse::<ResourceType>().unwrap(), held);
    }

    #[test]
    fn a_published_name_is_not_registered_again() {
        let held = register("Patient").expect("a published type");
        assert!(!held.is_custom());
    }

    #[test]
    fn a_name_that_is_no_type_name_is_refused() {
        for held in ["", "widget", "Wid get", "Wid-get", "9Widget"] {
            assert!(register(held).is_err(), "{held:?}");
        }
    }

    #[test]
    fn a_registered_type_is_served_by_every_release() {
        register("Gadget").expect("a plausible name");
        for version in FhirVersion::ALL {
            assert!(
                ResourceType::served(version)
                    .iter()
                    .any(|held| held.as_str() == "Gadget"),
                "{version}"
            );
        }
    }

    #[test]
    fn registering_the_same_name_twice_is_the_same_type() {
        let one = register("Doohickey").unwrap();
        let other = register("Doohickey").unwrap();
        assert_eq!(one, other);
    }
}
