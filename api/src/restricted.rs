












use fhir_core::{Error, ResourceType};
use std::collections::BTreeSet;



#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restricted {
    types: BTreeSet<ResourceType>,
    parameters: BTreeSet<String>,
}

impl Restricted {
    pub fn everything() -> Restricted {
        Restricted::default()
    }

    
    
    
    pub fn parse<I, S, J, T>(types: I, parameters: J) -> Result<Restricted, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        J: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let mut held = Restricted::default();
        for name in types {
            let name = name.as_ref().trim();
            if name.is_empty() {
                continue;
            }
            held.types.insert(name.parse::<ResourceType>()?);
        }
        for name in parameters {
            let name = name.as_ref().trim();
            if !name.is_empty() {
                held.parameters.insert(name.to_owned());
            }
        }
        Ok(held)
    }

    pub fn is_on(&self) -> bool {
        !self.types.is_empty() || !self.parameters.is_empty()
    }

    
    
    pub fn serves(&self, resource_type: ResourceType) -> bool {
        
        
        self.types.is_empty() || self.types.contains(&resource_type) || resource_type.is_custom()
    }

    
    
    
    
    pub fn answers(&self, name: &str) -> bool {
        self.parameters.is_empty() || name.starts_with('_') || self.parameters.contains(name)
    }

    pub fn types(&self) -> Vec<ResourceType> {
        self.types.iter().copied().collect()
    }

    pub fn parameters(&self) -> Vec<&str> {
        self.parameters.iter().map(String::as_str).collect()
    }

    
    
    
    pub fn refuse(&self, resource_type: ResourceType) -> Error {
        Error::InvalidResourceType(format!(
            "{resource_type} is not among the types this instance serves"
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
    fn an_instance_naming_nothing_serves_everything() {
        let held = Restricted::everything();
        assert!(!held.is_on());
        assert!(held.serves(kind("Patient")));
        assert!(held.serves(kind("Medication")));
        assert!(held.answers("family"));
    }

    #[test]
    fn what_is_named_is_served_and_what_is_not_is_not() {
        let held = Restricted::parse(["Patient", "Observation"], ["family", "code"])
            .expect("the names are known");
        assert!(held.is_on());
        assert!(held.serves(kind("Patient")));
        assert!(!held.serves(kind("Medication")));
        assert!(held.answers("family"));
        assert!(!held.answers("gender"));
    }

    #[test]
    fn the_result_control_parameters_are_never_narrowed_away() {
        let held = Restricted::parse(["Patient"], ["family"]).expect("the names are known");
        for name in ["_count", "_sort", "_include", "_summary", "_id"] {
            assert!(held.answers(name), "{name} is how a client reads a page");
        }
    }

    #[test]
    fn naming_only_types_leaves_the_parameters_alone() {
        let held = Restricted::parse(["Patient"], Vec::<String>::new()).expect("a type");
        assert!(held.answers("gender"), "and the other way round");
        assert!(!held.serves(kind("Medication")));
    }

    #[test]
    fn a_type_the_release_does_not_publish_is_refused_at_startup() {
        assert!(Restricted::parse(["Nonsense"], Vec::<String>::new()).is_err());
    }
}
