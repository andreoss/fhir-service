use async_trait::async_trait;
use fhir_core::search::{Criterion, Filter, Modifier, Target};
use fhir_core::terminology::{ancestors, descendants, expand, Coding, Expansion, ExpansionRequest};
use fhir_core::{Catalogue, Error, FhirVersion, ResourceType, SearchValue};
use fhir_store::{ResourceStore, SearchQuery, Subsumption, Terminology};
use serde_json::Value;
use std::sync::Arc;

const VALUE_SET: &str = "ValueSet";
const CODE_SYSTEM: &str = "CodeSystem";
const CONCEPT_MAP: &str = "ConceptMap";

pub struct StoredTerminology {
    store: Arc<dyn ResourceStore>,
    catalogue: Arc<Catalogue>,
}

impl StoredTerminology {
    pub fn new(store: Arc<dyn ResourceStore>, version: FhirVersion) -> StoredTerminology {
        StoredTerminology {
            store,
            catalogue: Catalogue::shared(version),
        }
    }

    pub fn with_catalogue(self, catalogue: Arc<Catalogue>) -> StoredTerminology {
        StoredTerminology { catalogue, ..self }
    }

    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    fn published(&self, set: &Value, stored: &[Value]) -> Vec<Value> {
        let held: Vec<&str> = stored.iter().filter_map(|body| url_of(body)).collect();
        let mut found = Vec::new();
        for (url, version) in composed(set) {
            if held.contains(&url.as_str()) {
                continue;
            }
            if let Some(body) = self.catalogue.system(&url, version.as_deref()) {
                found.push(body.clone());
            }
        }
        found
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

    async fn set(&self, url: &str, version: Option<&str>) -> Result<Value, Error> {
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
        match page.entries.first() {
            Some(found) => serde_json::from_slice(found.raw())
                .map_err(|error| Error::InvalidJson(error.to_string())),
            None => self
                .catalogue
                .set(url, version)
                .cloned()
                .ok_or(Error::NotFound),
        }
    }
}

#[async_trait]
impl Terminology for StoredTerminology {
    async fn expand(&self, url: &str, request: &ExpansionRequest) -> Result<Expansion, Error> {
        let set = self.set(url, request.value_set_version.as_deref()).await?;
        let mut systems = self.bodies(CODE_SYSTEM).await?;
        systems.extend(self.published(&set, &systems));
        expand(&set, &systems, request)
    }

    async fn code_system(&self, url: &str, version: Option<&str>) -> Result<Option<Value>, Error> {
        for body in self.bodies(CODE_SYSTEM).await? {
            if body.get("url").and_then(Value::as_str) == Some(url) {
                return Ok(Some(body));
            }
        }
        Ok(self.catalogue.system(url, version).cloned())
    }

    async fn value_set(&self, url: &str, version: Option<&str>) -> Result<Option<Value>, Error> {
        match self.set(url, version).await {
            Ok(held) => Ok(Some(held)),
            Err(Error::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn concept_maps(&self, url: Option<&str>) -> Result<Vec<Value>, Error> {
        let held = self.bodies(CONCEPT_MAP).await?;
        Ok(match url {
            None => held,
            Some(wanted) => held
                .into_iter()
                .filter(|body| body.get("url").and_then(Value::as_str) == Some(wanted))
                .collect(),
        })
    }

    async fn subsumption(
        &self,
        system: Option<&str>,
        code: &str,
        direction: Subsumption,
    ) -> Result<Vec<Coding>, Error> {
        let mut found = Vec::new();
        let mut stored = Vec::new();
        for body in self.bodies(CODE_SYSTEM).await? {
            let url = body.get("url").and_then(Value::as_str);
            if system.is_some_and(|wanted| url != Some(wanted)) {
                continue;
            }
            if let Some(url) = url {
                stored.push(url.to_owned());
            }
            let held = match direction {
                Subsumption::Below => descendants(&body, code),
                Subsumption::Above => ancestors(&body, code),
            };
            found.extend(held);
        }
        let published = match direction {
            Subsumption::Below => self.catalogue.descendants(system, code),
            Subsumption::Above => self.catalogue.ancestors(system, code),
        };
        found.extend(published.into_iter().filter(|concept| {
            concept
                .system
                .as_deref()
                .is_none_or(|url| !stored.iter().any(|held| held == url))
        }));
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
            return Err(Error::UnsupportedParameter(format!(
                "no code system defines {code:?}, so it is subsumed by nothing"
            )));
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

pub async fn resolve(terminology: &dyn Terminology, query: &mut SearchQuery) -> Result<(), Error> {
    for filter in &mut query.filters {
        if let Some(found) = subsumed(terminology, filter).await? {
            *filter = found;
        }
    }
    for chain in &mut query.chains {
        walked(terminology, &mut chain.next).await?;
    }
    Ok(())
}

async fn walked(terminology: &dyn Terminology, criterion: &mut Criterion) -> Result<(), Error> {
    let mut pending = vec![criterion];
    while let Some(held) = pending.pop() {
        match held {
            Criterion::Direct(filter) => {
                if let Some(found) = subsumed(terminology, filter).await? {
                    *filter = found;
                }
            }
            Criterion::Linked(chain) => pending.push(&mut chain.next),
        }
    }
    Ok(())
}

fn url_of(body: &Value) -> Option<&str> {
    body.get("url").and_then(Value::as_str)
}

fn composed(set: &Value) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    for name in ["include", "exclude"] {
        let rules = set
            .get("compose")
            .and_then(|compose| compose.get(name))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for rule in rules {
            let Some(url) = url_of(&rule).or_else(|| rule.get("system").and_then(Value::as_str))
            else {
                continue;
            };
            let base = url.split('|').next().unwrap_or(url).to_owned();
            let version = rule
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| url.split_once('|').map(|(_, held)| held.to_owned()));
            found.push((base, version));
        }
    }
    found
}
