use async_trait::async_trait;
use fhir_core::search::{Filter, Modifier, Target};
use fhir_core::terminology::{ancestors, descendants, expand, Coding, Expansion, ExpansionRequest};
use fhir_core::{Error, ResourceType, SearchValue};
use fhir_store::{ResourceStore, SearchQuery, Subsumption, Terminology};
use serde_json::Value;
use std::sync::Arc;

const VALUE_SET: &str = "ValueSet";
const CODE_SYSTEM: &str = "CodeSystem";

pub struct StoredTerminology {
    store: Arc<dyn ResourceStore>,
}

impl StoredTerminology {
    pub fn new(store: Arc<dyn ResourceStore>) -> StoredTerminology {
        StoredTerminology { store }
    }

    async fn bodies(&self, type_name: &str) -> Result<Vec<Value>, Error> {
        let resource_type = type_name.parse::<ResourceType>()?;
        let query = SearchQuery::of_type(resource_type);
        let page = self.store.search(&query).await?;
        page.entries
            .iter()
            .map(|found| {
                serde_json::from_slice(found.raw())
                    .map_err(|error| Error::InvalidJson(error.to_string()))
            })
            .collect()
    }

    async fn set(&self, url: &str) -> Result<Value, Error> {
        let resource_type = VALUE_SET.parse::<ResourceType>()?;
        let query = SearchQuery {
            types: vec![resource_type],
            filters: vec![Filter::new(
                "url",
                Target::path(["url"]),
                vec![SearchValue::parse(fhir_core::ValueType::Uri, url)?],
            )],
            ..SearchQuery::default()
        };
        let page = self.store.search(&query).await?;
        let found = page.entries.first().ok_or(Error::NotFound)?;
        serde_json::from_slice(found.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
    }
}

#[async_trait]
impl Terminology for StoredTerminology {
    async fn expand(&self, url: &str, request: &ExpansionRequest) -> Result<Expansion, Error> {
        let set = self.set(url).await?;
        let systems = self.bodies(CODE_SYSTEM).await?;
        expand(&set, &systems, request)
    }

    async fn subsumption(
        &self,
        system: Option<&str>,
        code: &str,
        direction: Subsumption,
    ) -> Result<Vec<Coding>, Error> {
        let mut found = Vec::new();
        for body in self.bodies(CODE_SYSTEM).await? {
            let url = body.get("url").and_then(Value::as_str);
            if system.is_some_and(|wanted| url != Some(wanted)) {
                continue;
            }
            let held = match direction {
                Subsumption::Below => descendants(&body, code),
                Subsumption::Above => ancestors(&body, code),
            };
            found.extend(held);
        }
        Ok(found)
    }
}

pub async fn subsumed(
    terminology: &dyn Terminology,
    filter: &Filter,
) -> Result<Option<Filter>, Error> {
    let direction = match filter.modifier {
        Modifier::Below => Subsumption::Below,
        Modifier::Above => Subsumption::Above,
        _ => return Ok(None),
    };
    let mut values = Vec::new();
    for value in &filter.values {
        let SearchValue::Token(token) = value else {
            return Ok(None);
        };
        let Some(code) = token.code.as_deref() else {
            return Ok(None);
        };
        let system = match &token.system {
            fhir_core::search::TokenSystem::Exact(text) => Some(text.as_str()),
            _ => None,
        };
        let found = terminology.subsumption(system, code, direction).await?;
        if found.is_empty() {
            return Ok(None);
        }
        values.extend(found.into_iter().map(coded));
    }
    Ok(Some(filter.expanded(&values)))
}

fn coded(concept: Coding) -> SearchValue {
    SearchValue::Token(fhir_core::search::Token {
        system: match concept.system {
            Some(text) => fhir_core::search::TokenSystem::Exact(text),
            None => fhir_core::search::TokenSystem::Any,
        },
        code: Some(concept.code),
    })
}

