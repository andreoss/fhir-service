use crate::ResourceType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub name: String,
    pub source: Option<ResourceType>,
    pub paths: Vec<String>,
    pub target: Option<ResourceType>,
    pub direction: IncludeDirection,
    pub iterate: bool,
}

impl Include {
    pub fn is_wildcard(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn covers(&self, resource_type: ResourceType) -> bool {
        self.source.is_none_or(|source| source == resource_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::lookup;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("the type is published")
    }

    fn rule(
        name: &str,
        source: Option<&str>,
        paths: Vec<String>,
        target: Option<&str>,
        direction: IncludeDirection,
        iterate: bool,
    ) -> Include {
        Include {
            name: name.to_owned(),
            source: source.map(kind),
            paths,
            target: target.map(kind),
            direction,
            iterate,
        }
    }

    fn named(source: &str, name: &str) -> Vec<String> {
        lookup(Some(kind(source)), name)
            .expect("the parameter is published")
            .paths()
    }

    #[test]
    fn a_rule_naming_no_path_follows_every_reference_of_a_match() {
        let every = rule(
            "_include",
            None,
            Vec::new(),
            None,
            IncludeDirection::Forward,
            false,
        );
        assert!(every.is_wildcard());
        assert!(every.covers(kind("Patient")));
        assert!(every.covers(kind("Observation")));
        let confined = rule(
            "_include",
            Some("Observation"),
            Vec::new(),
            None,
            IncludeDirection::Forward,
            false,
        );
        assert!(confined.is_wildcard());
        assert!(confined.covers(kind("Observation")));
        assert!(!confined.covers(kind("Patient")));
    }

    #[test]
    fn a_rule_naming_a_parameter_follows_the_element_that_parameter_reads() {
        let held = rule(
            "_include",
            Some("Observation"),
            named("Observation", "subject"),
            None,
            IncludeDirection::Forward,
            false,
        );
        assert!(!held.is_wildcard());
        assert_eq!(held.paths, vec!["subject".to_owned()]);
        assert!(held.covers(kind("Observation")));
        assert!(!held.covers(kind("Patient")));
    }

    #[test]
    fn a_rule_may_restrict_the_type_it_pulls_in() {
        let restricted = rule(
            "_include",
            Some("Observation"),
            named("Observation", "subject"),
            Some("Patient"),
            IncludeDirection::Forward,
            false,
        );
        assert_eq!(restricted.target, Some(kind("Patient")));
        let declared = lookup(Some(kind("Observation")), "subject")
            .expect("the parameter is published")
            .targets
            .clone();
        assert!(declared.contains(&"Patient".to_owned()));
        let open = rule(
            "_include",
            Some("Observation"),
            named("Observation", "subject"),
            None,
            IncludeDirection::Forward,
            false,
        );
        assert_eq!(open.target, None);
    }

    #[test]
    fn a_reverse_rule_names_the_type_that_points_at_the_match() {
        let reverse = rule(
            "_revinclude",
            Some("Observation"),
            named("Observation", "patient"),
            None,
            IncludeDirection::Reverse,
            false,
        );
        assert_eq!(reverse.direction, IncludeDirection::Reverse);
        assert!(reverse.covers(kind("Observation")));
        assert!(!reverse.covers(kind("Patient")));
        assert_eq!(reverse.paths, vec!["subject".to_owned()]);
    }

    #[test]
    fn a_rule_that_iterates_is_held_apart_from_one_applied_once() {
        let once = rule(
            "_include",
            Some("Observation"),
            named("Observation", "subject"),
            None,
            IncludeDirection::Forward,
            false,
        );
        let again = rule(
            "_include:iterate",
            Some("Patient"),
            named("Patient", "organization"),
            None,
            IncludeDirection::Forward,
            true,
        );
        assert!(!once.iterate);
        assert!(again.iterate);
        assert_ne!(once, again);
        let recurse = rule(
            "_include:recurse",
            Some("Patient"),
            named("Patient", "organization"),
            None,
            IncludeDirection::Forward,
            true,
        );
        assert_eq!(recurse.iterate, again.iterate);
        assert_eq!(recurse.paths, again.paths);
    }
}
