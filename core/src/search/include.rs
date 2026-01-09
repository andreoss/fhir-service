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

    fn rule(source: Option<&str>, paths: Vec<&str>) -> Include {
        Include {
            name: "_include".to_owned(),
            source: source.map(|text| text.parse().unwrap()),
            paths: paths.into_iter().map(str::to_owned).collect(),
            target: None,
            direction: IncludeDirection::Forward,
            iterate: false,
        }
    }

    #[test]
    fn a_rule_without_paths_follows_every_reference() {
        assert!(rule(None, Vec::new()).is_wildcard());
        assert!(!rule(Some("Observation"), vec!["subject"]).is_wildcard());
    }

    #[test]
    fn a_rule_without_a_source_covers_every_type() {
        let any = rule(None, Vec::new());
        assert!(any.covers("Patient".parse().unwrap()));
        let one = rule(Some("Observation"), vec!["subject"]);
        assert!(one.covers("Observation".parse().unwrap()));
        assert!(!one.covers("Patient".parse().unwrap()));
    }
}
