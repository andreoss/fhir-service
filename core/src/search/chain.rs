use crate::search::registry::Target;
use crate::search::Filter;
use crate::ResourceType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Criterion {
    Direct(Filter),
    Linked(Chain),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    
    
    pub name: String,
    
    
    
    
    pub link: String,
    pub target: Target,
    pub types: Vec<ResourceType>,
    pub direction: ChainDirection,
    pub next: Box<Criterion>,
}

impl Chain {
    pub fn depth(&self) -> usize {
        match self.next.as_ref() {
            Criterion::Direct(_) => 1,
            Criterion::Linked(chain) => 1 + chain.depth(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{lookup, pointers, select, SearchValue, ValueType};
    use serde_json::json;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("the type is published")
    }

    fn direct(name: &str, path: &str, raw: &str) -> Criterion {
        Criterion::Direct(Filter::new(
            name,
            Target::path([path]),
            vec![SearchValue::parse(ValueType::String, raw).unwrap()],
        ))
    }

    fn link(resource_type: &str, name: &str, direction: ChainDirection, next: Criterion) -> Chain {
        let def = lookup(Some(kind(resource_type)), name).expect("the parameter is published");
        Chain {
            name: format!("{name}.{}", spelled(&next)),
            link: name.to_owned(),
            target: def.target.clone(),
            types: def.targets.iter().map(|held| kind(held)).collect(),
            direction,
            next: Box::new(next),
        }
    }

    fn spelled(criterion: &Criterion) -> String {
        match criterion {
            Criterion::Direct(filter) => filter.name.clone(),
            Criterion::Linked(chain) => chain.name.clone(),
        }
    }

    #[test]
    fn a_chain_reaches_one_resource_for_every_link_it_names() {
        let one = link(
            "Observation",
            "subject",
            ChainDirection::Forward,
            direct("name", "name", "Ann"),
        );
        assert_eq!(one.depth(), 1);
        assert_eq!(one.name, "subject.name");
        let two = link(
            "Observation",
            "subject",
            ChainDirection::Forward,
            Criterion::Linked(link(
                "Patient",
                "organization",
                ChainDirection::Forward,
                direct("name", "name", "Ann"),
            )),
        );
        assert_eq!(two.depth(), 2);
        assert_eq!(two.name, "subject.organization.name");
        let three = link(
            "Observation",
            "subject",
            ChainDirection::Forward,
            Criterion::Linked(two),
        );
        assert_eq!(three.depth(), 3);
    }

    #[test]
    fn a_link_names_the_types_the_published_parameter_may_point_at() {
        let held = link(
            "Observation",
            "subject",
            ChainDirection::Forward,
            direct("name", "name", "Ann"),
        );
        assert_eq!(held.types, vec![kind("Patient"), kind("Group")]);
        let single = link(
            "Observation",
            "patient",
            ChainDirection::Forward,
            direct("name", "name", "Ann"),
        );
        assert_eq!(single.types, vec![kind("Patient")]);
    }

    #[test]
    fn a_forward_link_reads_the_reference_off_the_resource_under_test() {
        let held = link(
            "Observation",
            "subject",
            ChainDirection::Forward,
            direct("name", "name", "Ann"),
        );
        assert_eq!(held.direction, ChainDirection::Forward);
        let body = json!({
            "resourceType": "Observation",
            "subject": {"reference": "Patient/pt-1"}
        });
        let Target::Path(paths) = &held.target else {
            panic!("a chain over a reference reads paths")
        };
        let found: Vec<String> = paths
            .iter()
            .flat_map(|path| select(&body, path))
            .flat_map(pointers)
            .collect();
        assert_eq!(found, vec!["Patient/pt-1".to_owned()]);
    }

    #[test]
    fn a_reverse_link_reads_the_reference_off_the_resource_pointing_back() {
        let held = link(
            "Observation",
            "patient",
            ChainDirection::Reverse,
            direct("code", "code", "8867-4"),
        );
        assert_eq!(held.direction, ChainDirection::Reverse);
        let pointing = json!({
            "resourceType": "Observation",
            "subject": {"reference": "Patient/pt-1"}
        });
        let Target::Path(paths) = &held.target else {
            panic!("a chain over a reference reads paths")
        };
        let found: Vec<String> = paths
            .iter()
            .flat_map(|path| select(&pointing, path))
            .flat_map(pointers)
            .collect();
        assert_eq!(found, vec!["Patient/pt-1".to_owned()]);
        let unrelated = json!({"resourceType": "Patient", "id": "pt-1"});
        let none: Vec<String> = paths
            .iter()
            .flat_map(|path| select(&unrelated, path))
            .flat_map(pointers)
            .collect();
        assert!(none.is_empty());
    }

    #[test]
    fn the_condition_at_the_far_end_holds_of_the_far_resource_and_not_the_near_one() {
        let held = link(
            "Observation",
            "subject",
            ChainDirection::Forward,
            direct("name", "name", "Ann"),
        );
        let Criterion::Direct(filter) = held.next.as_ref() else {
            panic!("the last link carries a condition")
        };
        let id = crate::ResourceId::parse("pt-1").unwrap();
        let updated = crate::FhirInstant::parse("2026-09-06T04:00:00Z").unwrap();
        let far = json!({"resourceType": "Patient", "id": "pt-1", "name": [{"family": "Ann"}]});
        let near = json!({"resourceType": "Observation", "subject": {"reference": "Patient/pt-1"}});
        assert!(filter.matches(&id, &updated, &far));
        assert!(!filter.matches(&id, &updated, &near));
    }
}
