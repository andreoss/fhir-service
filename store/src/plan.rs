use crate::search::SearchQuery;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlanKey(String);

impl PlanKey {
    pub fn of(query: &SearchQuery) -> PlanKey {
        let mut parts: Vec<String> = query
            .types
            .iter()
            .map(|kind| format!("t:{}", kind.as_str()))
            .collect();
        parts.extend(
            query
                .filters
                .iter()
                .map(|filter| format!("f:{}:{:?}", filter.name, filter.modifier)),
        );
        parts.extend(query.chains.iter().map(|chain| format!("c:{}", chain.name)));
        parts.extend(query.includes.iter().map(|rule| format!("i:{}", rule.name)));
        parts.extend(
            query
                .sort
                .iter()
                .map(|key| format!("s:{}:{:?}", key.name, key.direction)),
        );
        if query.list.is_some() {
            parts.push("l".to_owned());
        }
        if let Some(compartment) = &query.compartment {
            parts.push(format!("m:{}", compartment.kind.as_str()));
        }
        if let Some(grant) = &query.grant {
            parts.push(format!("g:{}:{}", grant.types.len(), grant.compartments.len()));
        }
        PlanKey(parts.join("|"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Scan,
    Indexed {
        parameter: String,
    },
}

impl Plan {
    pub fn is_indexed(&self) -> bool {
        matches!(self, Plan::Indexed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStat {
    pub key: String,
    pub indexed: bool,
    pub uses: u64,
    pub baseline: u64,
    pub disabled: bool,
}

struct Entry {
    plan: Plan,
    uses: u64,
    baseline: Option<u64>,
    disabled: bool,
}

pub const REGRESSION_FACTOR: u64 = 4;

#[derive(Default)]
pub struct PlanCache {
    entries: RwLock<HashMap<String, Entry>>,
}

impl PlanCache {
    pub fn new() -> PlanCache {
        PlanCache::default()
    }

    pub fn chosen(&self, key: &PlanKey, proposed: Plan) -> Plan {
        let Ok(mut entries) = self.entries.write() else {
            return Plan::Scan;
        };
        let entry = entries.entry(key.0.clone()).or_insert_with(|| Entry {
            plan: proposed,
            uses: 0,
            baseline: None,
            disabled: false,
        });
        entry.uses += 1;
        match entry.disabled {
            true => Plan::Scan,
            false => entry.plan.clone(),
        }
    }

    pub fn observed(&self, key: &PlanKey, cost: u64) {
        let Ok(mut entries) = self.entries.write() else {
            return;
        };
        let Some(entry) = entries.get_mut(&key.0) else {
            return;
        };
        if entry.disabled {
            return;
        }
        match entry.baseline {
            None => entry.baseline = Some(cost),
            Some(baseline) if cost > baseline.max(1).saturating_mul(REGRESSION_FACTOR) => {
                entry.disabled = true;
            }
            Some(_) => {}
        }
    }

    pub fn stats(&self) -> Vec<PlanStat> {
        let Ok(entries) = self.entries.read() else {
            return Vec::new();
        };
        entries
            .iter()
            .map(|(key, entry)| PlanStat {
                key: key.clone(),
                indexed: entry.plan.is_indexed(),
                uses: entry.uses,
                baseline: entry.baseline.unwrap_or_default(),
                disabled: entry.disabled,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::ResourceType;

    fn key(name: &str) -> PlanKey {
        let kind: ResourceType = name.parse().unwrap();
        PlanKey::of(&SearchQuery::of_type(kind))
    }

    fn indexed() -> Plan {
        Plan::Indexed {
            parameter: "_id".to_owned(),
        }
    }

    #[test]
    fn a_shape_keeps_the_plan_it_was_first_given() {
        let cache = PlanCache::new();
        let key = key("Patient");
        assert_eq!(cache.chosen(&key, indexed()), indexed());
        assert_eq!(cache.chosen(&key, Plan::Scan), indexed());
        assert_eq!(cache.stats()[0].uses, 2);
    }

    #[test]
    fn two_shapes_do_not_share_a_plan() {
        let cache = PlanCache::new();
        cache.chosen(&key("Patient"), indexed());
        cache.chosen(&key("Observation"), Plan::Scan);
        assert_eq!(cache.stats().len(), 2);
    }

    #[test]
    fn a_regressed_plan_is_withdrawn_once_and_stays_withdrawn() {
        let cache = PlanCache::new();
        let key = key("Patient");
        cache.chosen(&key, indexed());
        cache.observed(&key, 2);
        cache.chosen(&key, indexed());
        cache.observed(&key, 9);
        assert_eq!(cache.chosen(&key, indexed()), Plan::Scan);
        cache.observed(&key, 1);
        assert!(cache.stats()[0].disabled);
        assert_eq!(cache.stats()[0].baseline, 2);
    }

    #[test]
    fn a_steady_plan_is_kept() {
        let cache = PlanCache::new();
        let key = key("Patient");
        cache.chosen(&key, indexed());
        cache.observed(&key, 3);
        cache.chosen(&key, indexed());
        cache.observed(&key, 12);
        assert_eq!(cache.chosen(&key, indexed()), indexed());
        assert!(!cache.stats()[0].disabled);
    }

    #[test]
    fn an_unknown_shape_records_nothing() {
        let cache = PlanCache::new();
        cache.observed(&key("Patient"), 5);
        assert!(cache.stats().is_empty());
        assert!(!key("Patient").as_str().is_empty());
    }
}
