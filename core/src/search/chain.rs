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
    use crate::search::{lookup, Modifier, SearchValue, ValueType};

    fn direct() -> Criterion {
        Criterion::Direct(Filter::new(
            "name",
            Target::path(["name"]),
            vec![SearchValue::parse(ValueType::String, "Ann").unwrap()],
        ))
    }

    fn link(next: Criterion) -> Chain {
        let def = lookup(Some("Observation".parse().unwrap()), "subject").unwrap();
        Chain {
            name: "subject.name".to_owned(),
            target: def.target.clone(),
            types: vec!["Patient".parse().unwrap()],
            direction: ChainDirection::Forward,
            next: Box::new(next),
        }
    }

    #[test]
    fn a_chain_reports_how_far_it_reaches() {
        assert_eq!(link(direct()).depth(), 1);
        assert_eq!(link(Criterion::Linked(link(direct()))).depth(), 2);
    }

    #[test]
    fn a_link_carries_the_condition_beyond_it() {
        let chain = link(direct());
        assert_eq!(chain.direction, ChainDirection::Forward);
        assert!(matches!(chain.next.as_ref(), Criterion::Direct(filter) if filter.modifier == Modifier::None));
    }
}
