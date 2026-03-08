use fhir_core::{InstantPeriod, ResourceEnvelope, ResourceId, ResourceType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryScope {
    System,
    Type(ResourceType),
    Instance(ResourceType, ResourceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOrder {
    Newest,
    Oldest,
}

#[derive(Debug, Clone)]
pub struct HistoryQuery {
    pub since: Option<InstantPeriod>,
    pub at: Option<InstantPeriod>,
    pub before: Option<InstantPeriod>,
    pub order: HistoryOrder,
    pub offset: usize,
    pub count: usize,
}

impl Default for HistoryQuery {
    fn default() -> HistoryQuery {
        HistoryQuery {
            since: None,
            at: None,
            before: None,
            order: HistoryOrder::Newest,
            offset: 0,
            count: usize::MAX,
        }
    }
}

impl HistoryQuery {
    pub fn keeps(&self, envelope: &ResourceEnvelope) -> bool {
        let key = envelope.last_updated().key();
        if self.since.is_some_and(|period| key < period.low()) {
            return false;
        }
        if self.before.is_some_and(|period| key >= period.low()) {
            return false;
        }
        if self
            .at
            .is_some_and(|period| !period.contains(envelope.last_updated()))
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub entries: Vec<ResourceEnvelope>,
    pub total: usize,
    pub offset: usize,
}
