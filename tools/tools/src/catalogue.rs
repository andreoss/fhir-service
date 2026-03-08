use fhir_core::{Error, FhirVersion};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct Content {
    bundles: Vec<Value>,
    published: Vec<Value>,
    sources: Vec<(String, String)>,
}

impl Content {
    pub fn read(artifacts: &Path, package: &Path, version: FhirVersion) -> Result<Content, Error> {
        let named = artifacts.join(format!("{}-valuesets.json", version.as_str()));
        let text = std::fs::read_to_string(&named)
            .map_err(|_| Error::Config(format!("cannot read {}", named.display())))?;
        let bundle = parsed(&text)?;
        let mut published = Vec::new();
        let listed = std::fs::read_dir(package)
            .map_err(|_| Error::Config(format!("cannot read {}", package.display())))?;
        let mut names: Vec<std::path::PathBuf> = listed
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("CodeSystem-") && name.ends_with(".json"))
            })
            .collect();
        names.sort();
        for path in names {
            let text = std::fs::read_to_string(&path)
                .map_err(|_| Error::Config(format!("cannot read {}", path.display())))?;
            published.push(parsed(&text)?);
        }
        let release = package.join("package.json");
        let identity = std::fs::read_to_string(&release)
            .ok()
            .and_then(|text| parsed(&text).ok())
            .map(|held| {
                (
                    text_at(&held, "name").unwrap_or_default().to_owned(),
                    text_at(&held, "version").unwrap_or_default().to_owned(),
                )
            })
            .unwrap_or_default();
        Ok(Content {
            sources: vec![
                (
                    format!("{}-valuesets", version.as_str()),
                    version.release().to_owned(),
                ),
                identity,
            ],
            bundles: vec![bundle],
            published,
        })
    }

    pub fn of(
        bundles: Vec<Value>,
        published: Vec<Value>,
        sources: Vec<(String, String)>,
    ) -> Content {
        Content {
            bundles,
            published,
            sources,
        }
    }
}

pub fn catalogue(version: FhirVersion, content: &Content) -> Result<String, Error> {
    let mut held: BTreeMap<(String, String), Value> = BTreeMap::new();
    for bundle in &content.bundles {
        for resource in entries(bundle) {
            keep(resource, &mut held);
        }
    }
    for resource in &content.published {
        keep(resource, &mut held);
    }
    if held.is_empty() {
        return Err(Error::Config("the content names no code system".to_owned()));
    }
    let mut systems = Vec::new();
    let mut unsupplied = Vec::new();
    for ((url, version), resource) in &held {
        let kind = text_at(resource, "content").unwrap_or("not-present");
        let listed = trimmed(resource.get("concept"));
        match kind == "complete" && !listed.is_empty() {
            true => systems.push(described(url, version, listed)),
            false => unsupplied.push(serde_json::json!({
                "url": url,
                "version": version,
                "content": kind,
                "reason": match kind == "complete" {
                    true => "the publication carries no concepts",
                    false => "the content is published elsewhere",
                }
            })),
        }
    }
    if systems.is_empty() {
        return Err(Error::Config(
            "the content carries no complete system".to_owned(),
        ));
    }
    let named: BTreeSet<String> = held.keys().map(|(url, _)| url.clone()).collect();
    for url in referenced(content) {
        if named.contains(&url) {
            continue;
        }
        unsupplied.push(serde_json::json!({
            "url": url,
            "version": "",
            "content": "referenced",
            "reason": "the publication names the system and carries no content for it"
        }));
    }
    let file = serde_json::json!({
        "version": version.as_str(),
        "release": version.release(),
        "sources": content
            .sources
            .iter()
            .map(|(name, held)| serde_json::json!({"name": name, "version": held}))
            .collect::<Vec<Value>>(),
        "systems": systems,
        "unsupplied": unsupplied,
    });
    let mut text =
        serde_json::to_string(&file).map_err(|reason| Error::Internal(reason.to_string()))?;
    text.push('\n');
    Ok(text)
}

fn described(url: &str, version: &str, concepts: Vec<Value>) -> Value {
    let mut held = Map::new();
    held.insert("url".to_owned(), Value::String(url.to_owned()));
    if !version.is_empty() {
        held.insert("version".to_owned(), Value::String(version.to_owned()));
    }
    held.insert("content".to_owned(), Value::String("complete".to_owned()));
    held.insert("concept".to_owned(), Value::Array(concepts));
    Value::Object(held)
}

fn keep(resource: &Value, held: &mut BTreeMap<(String, String), Value>) {
    if text_at(resource, "resourceType") != Some("CodeSystem") {
        return;
    }
    let Some(url) = text_at(resource, "url") else {
        return;
    };
    let version = text_at(resource, "version").unwrap_or_default();
    let key = (url.to_owned(), version.to_owned());
    let complete = text_at(resource, "content") == Some("complete");
    let standing = held
        .get(&key)
        .map(|found| text_at(found, "content") == Some("complete"))
        .unwrap_or(false);
    if standing && !complete {
        return;
    }
    held.insert(key, resource.clone());
}

fn referenced(content: &Content) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut consider = |resource: &Value| {
        if text_at(resource, "resourceType") != Some("ValueSet") {
            return;
        }
        for name in ["include", "exclude"] {
            let rules = resource
                .get("compose")
                .and_then(|compose| compose.get(name))
                .and_then(Value::as_array)
                .map(|items| items.as_slice())
                .unwrap_or(&[])
                .to_vec();
            for rule in rules {
                if let Some(url) = text_at(&rule, "system") {
                    found.insert(url.split('|').next().unwrap_or(url).to_owned());
                }
            }
        }
    };
    for bundle in &content.bundles {
        for resource in entries(bundle) {
            consider(resource);
        }
    }
    for resource in &content.published {
        consider(resource);
    }
    found
}

fn entries(bundle: &Value) -> impl Iterator<Item = &Value> {
    bundle
        .get("entry")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| entry.get("resource"))
}

fn trimmed(held: Option<&Value>) -> Vec<Value> {
    let Some(items) = held.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|concept| {
            let code = text_at(concept, "code")?;
            let mut held = Map::new();
            held.insert("code".to_owned(), Value::String(code.to_owned()));
            if let Some(display) = text_at(concept, "display") {
                held.insert("display".to_owned(), Value::String(display.to_owned()));
            }
            if retired(concept) {
                held.insert("inactive".to_owned(), Value::Bool(true));
            }
            let under = trimmed(concept.get("concept"));
            if !under.is_empty() {
                held.insert("concept".to_owned(), Value::Array(under));
            }
            Some(Value::Object(held))
        })
        .collect()
}

fn retired(concept: &Value) -> bool {
    if concept.get("inactive").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    concept
        .get("property")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
        .iter()
        .any(|property| {
            let code = text_at(property, "code");
            let value = text_at(property, "valueCode").or_else(|| text_at(property, "valueString"));
            let flag = property.get("valueBoolean").and_then(Value::as_bool);
            matches!(
                (code, value, flag),
                (Some("status"), Some("retired" | "deprecated"), _)
                    | (Some("inactive"), _, Some(true))
            )
        })
}

fn parsed(text: &str) -> Result<Value, Error> {
    serde_json::from_str(text).map_err(|reason| Error::InvalidJson(reason.to_string()))
}

fn text_at<'a>(held: &'a Value, name: &str) -> Option<&'a str> {
    held.get(name).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> Value {
        serde_json::json!({
            "resourceType": "Bundle",
            "entry": [
                {"resource": {
                    "resourceType": "CodeSystem",
                    "url": "urn:one",
                    "version": "1.0",
                    "content": "complete",
                    "concept": [{
                        "code": "top",
                        "display": "Top",
                        "concept": [{"code": "leaf", "property": [
                            {"code": "status", "valueCode": "retired"}
                        ]}]
                    }]
                }},
                {"resource": {
                    "resourceType": "CodeSystem",
                    "url": "urn:two",
                    "version": "2.0",
                    "content": "not-present"
                }},
                {"resource": {"resourceType": "ValueSet", "url": "urn:vs"}}
            ]
        })
    }

    fn content() -> Content {
        Content::of(
            vec![bundle()],
            Vec::new(),
            vec![("held".to_owned(), "1".to_owned())],
        )
    }

    #[test]
    fn a_complete_system_keeps_its_published_hierarchy() {
        let text = catalogue(FhirVersion::R4, &content()).unwrap();
        let held: Value = serde_json::from_str(&text).unwrap();
        let systems = held.get("systems").and_then(Value::as_array).unwrap();
        assert_eq!(systems.len(), 1);
        assert_eq!(text_at(&systems[0], "url"), Some("urn:one"));
        assert_eq!(text_at(&systems[0], "version"), Some("1.0"));
        let concepts = systems[0].get("concept").and_then(Value::as_array).unwrap();
        let under = concepts[0]
            .get("concept")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(text_at(&under[0], "code"), Some("leaf"));
        assert_eq!(under[0].get("inactive"), Some(&Value::Bool(true)));
    }

    #[test]
    fn a_system_published_elsewhere_is_recorded_unsupplied() {
        let text = catalogue(FhirVersion::R4, &content()).unwrap();
        let held: Value = serde_json::from_str(&text).unwrap();
        let listed = held.get("unsupplied").and_then(Value::as_array).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(text_at(&listed[0], "url"), Some("urn:two"));
        assert_eq!(text_at(&listed[0], "content"), Some("not-present"));
    }

    #[test]
    fn the_same_content_produces_the_same_bytes() {
        let first = catalogue(FhirVersion::R4, &content()).unwrap();
        let second = catalogue(FhirVersion::R4, &content()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn published_content_replaces_a_placeholder_of_the_same_version() {
        let published = serde_json::json!({
            "resourceType": "CodeSystem",
            "url": "urn:two",
            "version": "2.0",
            "content": "complete",
            "concept": [{"code": "only"}]
        });
        let held = Content::of(vec![bundle()], vec![published], Vec::new());
        let text = catalogue(FhirVersion::R4, &held).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let systems = parsed.get("systems").and_then(Value::as_array).unwrap();
        assert_eq!(systems.len(), 2);
        assert!(parsed
            .get("unsupplied")
            .and_then(Value::as_array)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn content_naming_no_code_system_is_refused() {
        let held = Content::of(
            vec![serde_json::json!({"entry": []})],
            Vec::new(),
            Vec::new(),
        );
        assert!(catalogue(FhirVersion::R4, &held).is_err());
    }
}

#[cfg(test)]
mod referenced_tests {
    use super::*;

    #[test]
    fn a_system_named_but_never_published_is_recorded_referenced() {
        let bundle = serde_json::json!({
            "entry": [
                {"resource": {
                    "resourceType": "CodeSystem",
                    "url": "urn:one",
                    "content": "complete",
                    "concept": [{"code": "a"}]
                }},
                {"resource": {
                    "resourceType": "ValueSet",
                    "url": "urn:vs",
                    "compose": {"include": [{"system": "urn:elsewhere|2.0"}, {"system": "urn:one"}]}
                }}
            ]
        });
        let held = Content::of(vec![bundle], Vec::new(), Vec::new());
        let text = catalogue(FhirVersion::R4, &held).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let listed = parsed.get("unsupplied").and_then(Value::as_array).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(text_at(&listed[0], "url"), Some("urn:elsewhere"));
        assert_eq!(text_at(&listed[0], "content"), Some("referenced"));
    }
}
