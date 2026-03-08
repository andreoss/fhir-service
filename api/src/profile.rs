use fhir_core::profile::{CodeSource, Profile};
use fhir_core::search::{Filter, SearchValue, Target, ValueType};
use fhir_core::terminology::{flattened, ExpansionRequest};
use fhir_core::{Error, ResourceType};
use fhir_store::{ResourceStore, SearchQuery, Terminology};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

const STRUCTURE_DEFINITION: &str = "StructureDefinition";



type Code = (Option<String>, String);




pub async fn resolve(store: &Arc<dyn ResourceStore>, url: &str) -> Result<Option<Profile>, Error> {
    let Ok(resource_type) = STRUCTURE_DEFINITION.parse::<ResourceType>() else {
        return Ok(None);
    };
    let query = SearchQuery {
        types: vec![resource_type],
        filters: vec![Filter::new(
            "url",
            Target::path(["url"]),
            vec![SearchValue::parse(ValueType::Uri, url)?],
        )],
        count: 2,
        ..SearchQuery::default()
    };
    let page = store.search(&query).await?;
    let Some(found) = page.entries.first() else {
        return Ok(None);
    };
    let body: Value = serde_json::from_slice(found.raw())
        .map_err(|error| Error::InvalidJson(error.to_string()))?;
    Profile::parse(&body).map(Some)
}








pub fn defines_a_type(definition: &Value) -> Option<String> {
    let object = definition.as_object()?;
    if object.get("resourceType").and_then(Value::as_str) != Some(STRUCTURE_DEFINITION) {
        return None;
    }
    let named = |name: &str, wanted: &str| -> bool {
        object.get(name).and_then(Value::as_str) == Some(wanted)
    };
    let base = object
        .get("baseDefinition")
        .and_then(Value::as_str)
        .and_then(|held| held.rsplit('/').next());
    let declares = base == Some("DomainResource")
        && named("derivation", "specialization")
        && named("kind", "resource")
        && object.get("abstract").and_then(Value::as_bool) == Some(false);
    match declares {
        true => object
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        false => None,
    }
}



pub async fn register_types(store: &Arc<dyn ResourceStore>) -> Result<Vec<String>, Error> {
    let Ok(resource_type) = STRUCTURE_DEFINITION.parse::<ResourceType>() else {
        return Ok(Vec::new());
    };
    let query = SearchQuery {
        types: vec![resource_type],
        count: usize::MAX,
        ..SearchQuery::default()
    };
    let page = store.search(&query).await?;
    let mut held = Vec::new();
    for entry in &page.entries {
        let body: Value = serde_json::from_slice(entry.raw())
            .map_err(|error| Error::InvalidJson(error.to_string()))?;
        if let Some(name) = defines_a_type(&body) {
            fhir_core::resource_type::register(&name)?;
            held.push(name);
        }
    }
    Ok(held)
}









#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedProfiles {
    held: Vec<String>,
}

impl AllowedProfiles {
    pub fn new(held: Vec<String>) -> AllowedProfiles {
        AllowedProfiles { held }
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn named(&self) -> &[String] {
        &self.held
    }

    
    pub fn accepts(&self, body: &Value) -> bool {
        if self.held.is_empty() {
            return true;
        }
        body.get("meta")
            .and_then(|meta| meta.get("profile"))
            .and_then(Value::as_array)
            .map(|items| {
                items.iter().filter_map(Value::as_str).any(|claimed| {
                    self.held
                        .iter()
                        .any(|allowed| allowed == claimed.split('|').next().unwrap_or(claimed))
                })
            })
            .unwrap_or(false)
    }
}




#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OnWrite {
    pub create: bool,
    pub update: bool,
}

impl OnWrite {
    pub fn parse(raw: &str) -> Result<OnWrite, Error> {
        let mut held = OnWrite::default();
        for part in raw
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            match part {
                "create" => held.create = true,
                "update" => held.update = true,
                "none" => held = OnWrite::default(),
                other => {
                    return Err(Error::Config(format!(
                        "profile validation {other:?} names neither create nor update"
                    )))
                }
            }
        }
        Ok(held)
    }

    pub fn asked(&self) -> bool {
        self.create || self.update
    }
}





pub async fn judged_for_write(
    store: &Arc<dyn ResourceStore>,
    terminology: &dyn Terminology,
    body: &Value,
) -> Result<(), Error> {
    let Some(url) = body
        .get("meta")
        .and_then(|meta| meta.get("profile"))
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find_map(Value::as_str))
    else {
        return Ok(());
    };
    if url.starts_with("http://hl7.org/fhir/StructureDefinition/") {
        return Ok(());
    }
    let Some(profile) = resolve(store, url).await? else {
        return Err(Error::NoMatch(format!(
            "profile {url:?} could not be resolved, so its rules cannot be applied to this write"
        )));
    };
    let codes = HeldCodes::for_profile(terminology, &profile).await?;
    let issues = profile.judge(body, &codes);
    let refused: Vec<String> = issues
        .iter()
        .filter(|issue| {
            matches!(
                issue.severity,
                fhir_core::IssueSeverity::Error | fhir_core::IssueSeverity::Fatal
            )
        })
        .map(|issue| issue.diagnostics.clone())
        .collect();
    match refused.is_empty() {
        true => Ok(()),
        false => Err(Error::NoMatch(refused.join("; "))),
    }
}




#[derive(Debug, Default)]
pub struct HeldCodes {
    sets: BTreeMap<String, Option<Vec<Code>>>,
}

impl HeldCodes {
    pub async fn for_profile(
        terminology: &dyn Terminology,
        profile: &Profile,
    ) -> Result<HeldCodes, Error> {
        let mut sets = BTreeMap::new();
        for constraint in profile.constraints() {
            let Some(binding) = &constraint.binding else {
                continue;
            };
            if !binding.is_enforced() || sets.contains_key(&binding.value_set) {
                continue;
            }
            let request = ExpansionRequest {
                count: None,
                ..ExpansionRequest::default()
            };
            let held = match terminology.expand(&binding.value_set, &request).await {
                Ok(expansion) => Some(
                    flattened(&expansion.concepts)
                        .into_iter()
                        .map(|coding| (coding.system.clone(), coding.code.clone()))
                        .collect(),
                ),
                Err(Error::NotFound) => None,
                Err(Error::UnsupportedParameter(_)) => None,
                Err(error) => return Err(error),
            };
            sets.insert(binding.value_set.clone(), held);
        }
        Ok(HeldCodes { sets })
    }
}

impl CodeSource for HeldCodes {
    fn codes(&self, value_set: &str) -> Option<Vec<Code>> {
        self.sets.get(value_set).cloned().flatten()
    }
}
