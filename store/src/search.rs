use fhir_core::search::{Chain, Compartment, Filter, Grant, Include};
use fhir_core::{ResourceEnvelope, ResourceId, ResourceType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotalMode {
    Accurate,
    Estimate,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub name: String,
    pub target: fhir_core::search::Target,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchQuery {
    pub types: Vec<ResourceType>,
    pub filters: Vec<Filter>,
    pub chains: Vec<Chain>,
    pub includes: Vec<Include>,
    pub list: Option<ResourceId>,
    pub compartment: Option<Compartment>,
    pub grant: Option<Grant>,
    pub sort: Vec<SortKey>,
    pub offset: usize,
    pub count: usize,
    pub total: TotalMode,
}

impl Default for SearchQuery {
    fn default() -> SearchQuery {
        SearchQuery {
            types: Vec::new(),
            filters: Vec::new(),
            chains: Vec::new(),
            includes: Vec::new(),
            list: None,
            compartment: None,
            grant: None,
            sort: Vec::new(),
            offset: 0,
            count: usize::MAX,
            total: TotalMode::Accurate,
        }
    }
}

impl SearchQuery {
    pub fn of_type(resource_type: ResourceType) -> SearchQuery {
        SearchQuery {
            types: vec![resource_type],
            ..SearchQuery::default()
        }
    }

    pub fn is_unconditional(&self) -> bool {
        self.filters.is_empty()
            && self.chains.is_empty()
            && self.list.is_none()
            && self.compartment.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    pub entries: Vec<ResourceEnvelope>,
    pub included: Vec<ResourceEnvelope>,
    pub total: Option<usize>,
    pub offset: usize,
}
