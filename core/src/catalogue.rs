use crate::fhir_version::FhirVersion;
use crate::terminology::{ancestors, descendants, Coding};
use crate::Error;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

const STU3: &str = include_str!("../terminology/stu3.json");
const R4: &str = include_str!("../terminology/r4.json");
const R4B: &str = include_str!("../terminology/r4b.json");
const R5: &str = include_str!("../terminology/r5.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupplied {
    pub url: String,
    pub version: String,
    pub content: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct Catalogue {
    version: FhirVersion,
    sources: Vec<Source>,
    systems: BTreeMap<(String, String), Value>,
    sets: BTreeMap<(String, String), Value>,
    unsupplied: Vec<Unsupplied>,
}

impl Catalogue {
    pub fn of(version: FhirVersion) -> &'static Catalogue {
        held(version).as_ref()
    }

    pub fn shared(version: FhirVersion) -> Arc<Catalogue> {
        Arc::clone(held(version))
    }

    pub fn parse(version: FhirVersion, text: &str) -> Result<Catalogue, Error> {
        let held: Value =
            serde_json::from_str(text).map_err(|reason| Error::InvalidJson(reason.to_string()))?;
        let mut systems = BTreeMap::new();
        for found in held
            .get("systems")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let Some(object) = found.as_object() else {
                continue;
            };
            let Some(url) = text_at(found, "url") else {
                continue;
            };
            systems.insert(
                (
                    url.to_owned(),
                    text_at(found, "version").unwrap_or_default().to_owned(),
                ),
                body(object),
            );
        }
        if systems.is_empty() {
            return Err(Error::Config(format!(
                "the catalogue of {version} holds no code system"
            )));
        }
        Ok(Catalogue {
            version,
            sources: held
                .get("sources")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(source).collect())
                .unwrap_or_default(),
            sets: BTreeMap::new(),
            systems,
            unsupplied: held
                .get("unsupplied")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(unsupplied).collect())
                .unwrap_or_default(),
        })
    }

    pub fn version(&self) -> FhirVersion {
        self.version
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    pub fn systems(&self) -> Vec<&Value> {
        self.systems.values().collect()
    }

    pub fn unsupplied(&self) -> &[Unsupplied] {
        &self.unsupplied
    }

    pub fn sets(&self) -> Vec<&Value> {
        self.sets.values().collect()
    }

    pub fn set(&self, url: &str, version: Option<&str>) -> Option<&Value> {
        match version {
            Some(wanted) => self.sets.get(&(url.to_owned(), wanted.to_owned())),
            None => self
                .sets
                .range((url.to_owned(), String::new())..)
                .take_while(|((held, _), _)| held == url)
                .last()
                .map(|(_, body)| body),
        }
    }

    pub fn system(&self, url: &str, version: Option<&str>) -> Option<&Value> {
        match version {
            Some(wanted) => self.systems.get(&(url.to_owned(), wanted.to_owned())),
            None => self
                .systems
                .range((url.to_owned(), String::new())..)
                .take_while(|((held, _), _)| held == url)
                .last()
                .map(|(_, body)| body),
        }
    }

    pub fn versions(&self, url: &str) -> Vec<&str> {
        self.systems
            .range((url.to_owned(), String::new())..)
            .take_while(|((held, _), _)| held == url)
            .map(|((_, version), _)| version.as_str())
            .collect()
    }

    pub fn with(&self, bodies: Vec<Value>) -> Catalogue {
        let mut held = self.clone();
        for body in bodies {
            if let Ok(loaded) = held.clone().loaded(&body) {
                held = loaded;
            }
        }
        held
    }

    pub fn loaded(mut self, body: &Value) -> Result<Catalogue, Error> {
        match text_at(body, "resourceType") {
            Some("ValueSet") => return self.loaded_set(body),
            Some("CodeSystem") => {}
            _ => {
                return Err(Error::InvalidParameter(
                    "supplied terminology is neither a code system nor a value set".to_owned(),
                ))
            }
        }
        let url = text_at(body, "url").ok_or_else(|| {
            Error::InvalidParameter("a supplied code system carries no url".to_owned())
        })?;
        if body
            .get("concept")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err(Error::InvalidParameter(format!(
                "the supplied code system {url:?} carries no concept"
            )));
        }
        let version = text_at(body, "version").unwrap_or_default().to_owned();
        if version.is_empty() {
            self.systems.retain(|(held, _), _| held != url);
        }
        self.systems.insert((url.to_owned(), version), body.clone());
        self.unsupplied.retain(|found| found.url != url);
        Ok(self)
    }

    fn loaded_set(mut self, body: &Value) -> Result<Catalogue, Error> {
        let url = text_at(body, "url").ok_or_else(|| {
            Error::InvalidParameter("a supplied value set carries no url".to_owned())
        })?;
        let defined = |name: &str| {
            body.get(name)
                .and_then(Value::as_object)
                .is_some_and(|held| !held.is_empty())
        };
        if !defined("compose") && !defined("expansion") {
            return Err(Error::InvalidParameter(format!(
                "the supplied value set {url:?} carries neither a compose nor an expansion"
            )));
        }
        let version = text_at(body, "version").unwrap_or_default().to_owned();
        if version.is_empty() {
            self.sets.retain(|(held, _), _| held != url);
        }
        self.sets.insert((url.to_owned(), version), body.clone());
        Ok(self)
    }

    pub fn descendants(&self, system: Option<&str>, code: &str) -> Vec<Coding> {
        self.walked(system, code, descendants)
    }

    pub fn ancestors(&self, system: Option<&str>, code: &str) -> Vec<Coding> {
        self.walked(system, code, ancestors)
    }

    fn walked(
        &self,
        system: Option<&str>,
        code: &str,
        walk: fn(&Value, &str) -> Vec<Coding>,
    ) -> Vec<Coding> {
        let mut found = Vec::new();
        for ((url, _), body) in &self.systems {
            if system.is_some_and(|wanted| url != wanted) {
                continue;
            }
            found.extend(walk(body, code));
        }
        found
    }
}

fn body(object: &Map<String, Value>) -> Value {
    let mut held = object.clone();
    held.insert(
        "resourceType".to_owned(),
        Value::String("CodeSystem".to_owned()),
    );
    Value::Object(held)
}

fn source(held: &Value) -> Option<Source> {
    Some(Source {
        name: text_at(held, "name")?.to_owned(),
        version: text_at(held, "version").unwrap_or_default().to_owned(),
    })
}

fn unsupplied(held: &Value) -> Option<Unsupplied> {
    Some(Unsupplied {
        url: text_at(held, "url")?.to_owned(),
        version: text_at(held, "version").unwrap_or_default().to_owned(),
        content: text_at(held, "content").unwrap_or_default().to_owned(),
        reason: text_at(held, "reason").unwrap_or_default().to_owned(),
    })
}

fn text_at<'a>(held: &'a Value, name: &str) -> Option<&'a str> {
    held.get(name).and_then(Value::as_str)
}

fn generated(version: FhirVersion) -> &'static str {
    match version {
        FhirVersion::Stu3 => STU3,
        FhirVersion::R4 => R4,
        FhirVersion::R4b => R4B,
        FhirVersion::R5 => R5,
    }
}

fn held(version: FhirVersion) -> &'static Arc<Catalogue> {
    holder(version).get_or_init(|| {
        Arc::new(
            Catalogue::parse(version, generated(version))
                .expect("a generated catalogue file is well formed"),
        )
    })
}

fn holder(version: FhirVersion) -> &'static OnceLock<Arc<Catalogue>> {
    static HELD_STU3: OnceLock<Arc<Catalogue>> = OnceLock::new();
    static HELD_R4: OnceLock<Arc<Catalogue>> = OnceLock::new();
    static HELD_R4B: OnceLock<Arc<Catalogue>> = OnceLock::new();
    static HELD_R5: OnceLock<Arc<Catalogue>> = OnceLock::new();
    match version {
        FhirVersion::Stu3 => &HELD_STU3,
        FhirVersion::R4 => &HELD_R4,
        FhirVersion::R4b => &HELD_R4B,
        FhirVersion::R5 => &HELD_R5,
    }
}
