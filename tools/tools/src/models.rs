use fhir_core::{Error, FhirVersion};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct Artifacts {
    resources: Value,
    types: Value,
    value_sets: Value,
}

impl Artifacts {
    pub fn read(directory: &Path, version: FhirVersion) -> Result<Artifacts, Error> {
        let named = |what: &str| directory.join(format!("{}-{what}.json", version.as_str()));
        let read = |path: std::path::PathBuf| {
            std::fs::read_to_string(&path)
                .map_err(|_| Error::Config(format!("cannot read {}", path.display())))
        };
        Artifacts::parse(
            &read(named("profiles-resources"))?,
            &read(named("profiles-types"))?,
            &read(named("valuesets"))?,
        )
    }

    pub fn parse(resources: &str, types: &str, value_sets: &str) -> Result<Artifacts, Error> {
        let parse = |text: &str| {
            serde_json::from_str::<Value>(text)
                .map_err(|reason| Error::InvalidJson(reason.to_string()))
        };
        Ok(Artifacts {
            resources: parse(resources)?,
            types: parse(types)?,
            value_sets: parse(value_sets)?,
        })
    }
}

struct Collected {
    nodes: BTreeMap<String, Vec<Value>>,
    resources: BTreeSet<String>,
    primitives: BTreeMap<String, Value>,
    profiles: BTreeMap<String, String>,
    wanted: BTreeSet<String>,
}

pub fn definitions(version: FhirVersion, artifacts: &Artifacts) -> Result<String, Error> {
    let mut collected = Collected {
        nodes: BTreeMap::new(),
        resources: BTreeSet::new(),
        primitives: BTreeMap::new(),
        profiles: BTreeMap::new(),
        wanted: BTreeSet::new(),
    };
    for definition in structures(&artifacts.types) {
        gather(definition, &mut collected);
    }
    for definition in structures(&artifacts.resources) {
        gather(definition, &mut collected);
    }
    if collected.resources.is_empty() {
        return Err(Error::Config(
            "the artifacts name no resource type".to_owned(),
        ));
    }
    let (bindings, unresolved) = codes(&artifacts.value_sets, &collected.wanted);
    let nodes: Map<String, Value> = collected
        .nodes
        .into_iter()
        .map(|(path, elements)| (path, Value::Array(elements)))
        .collect();
    let file = serde_json::json!({
        "version": version.as_str(),
        "release": version.release(),
        "resources": collected.resources.iter().collect::<Vec<_>>(),
        "primitives": Value::Object(collected.primitives.into_iter().collect()),
        "profiles": Value::Object(
            collected
                .profiles
                .into_iter()
                .map(|(url, name)| (url, Value::String(name)))
                .collect(),
        ),
        "nodes": Value::Object(nodes),
        "bindings": Value::Object(bindings.into_iter().collect()),
        "unresolved": unresolved.iter().collect::<Vec<_>>(),
    });
    let mut text = serde_json::to_string_pretty(&file)
        .map_err(|reason| Error::Internal(reason.to_string()))?;
    text.push('\n');
    Ok(text)
}

fn structures(bundle: &Value) -> impl Iterator<Item = &Value> {
    bundle
        .get("entry")
        .and_then(Value::as_array)
        .map(|entries| entries.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| entry.get("resource"))
        .filter(|resource| text_at(resource, "resourceType") == Some("StructureDefinition"))
        .filter(|resource| text_at(resource, "derivation") != Some("constraint"))
}

fn gather(definition: &Value, collected: &mut Collected) {
    let Some(name) = text_at(definition, "name") else {
        return;
    };
    let Some(kind) = text_at(definition, "kind") else {
        return;
    };
    let abstracted = definition.get("abstract").and_then(Value::as_bool) == Some(true);
    if kind == "resource" && !abstracted {
        collected.resources.insert(name.to_owned());
    }
    if kind == "primitive-type" {
        collected
            .primitives
            .insert(name.to_owned(), primitive(name, definition));
        return;
    }
    if kind != "resource" && kind != "complex-type" {
        return;
    }
    if let Some(url) = text_at(definition, "url") {
        collected.profiles.insert(url.to_owned(), name.to_owned());
    }
    let elements = definition
        .get("snapshot")
        .and_then(|snapshot| snapshot.get("element"))
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[]);
    for element in elements {
        let Some(path) = text_at(element, "path") else {
            continue;
        };
        let Some((parent, last)) = path.rsplit_once('.') else {
            collected.nodes.entry(name.to_owned()).or_default();
            continue;
        };
        if !path.starts_with(name) {
            continue;
        }
        let described = describe(last, element, collected);
        collected
            .nodes
            .entry(parent.to_owned())
            .or_default()
            .push(described);
    }
}

fn describe(name: &str, element: &Value, collected: &mut Collected) -> Value {
    let types: Vec<String> = element
        .get("type")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(code_of)
        .collect();
    let bound = binding(element);
    if let Some(url) = &bound {
        collected.wanted.insert(url.clone());
    }
    let mut described = Map::new();
    described.insert("n".to_owned(), Value::String(name.to_owned()));
    described.insert(
        "t".to_owned(),
        Value::Array(types.into_iter().map(Value::String).collect()),
    );
    described.insert("min".to_owned(), Value::from(minimum(element)));
    described.insert("max".to_owned(), Value::from(maximum(element)));
    if let Some(url) = bound {
        described.insert("b".to_owned(), Value::String(url));
    }
    if let Some(reference) = text_at(element, "contentReference") {
        described.insert(
            "r".to_owned(),
            Value::String(reference.trim_start_matches('#').to_owned()),
        );
    }
    Value::Object(described)
}

fn code_of(held: &Value) -> Option<String> {
    let code = text_at(held, "code")?;
    if let Some(named) = code.strip_prefix("http://hl7.org/fhirpath/System.") {
        return Some(match fhir_typed(held) {
            Some(name) => name,
            None => named.to_ascii_lowercase(),
        });
    }
    Some(code.to_owned())
}

fn fhir_typed(held: &Value) -> Option<String> {
    held.get("extension")
        .and_then(Value::as_array)?
        .iter()
        .find(|extension| {
            text_at(extension, "url")
                .map(|url| url.ends_with("structuredefinition-fhir-type"))
                .unwrap_or(false)
        })
        .and_then(|extension| text_at(extension, "valueUrl").or(text_at(extension, "valueString")))
        .map(str::to_owned)
}

fn binding(element: &Value) -> Option<String> {
    let held = element.get("binding")?;
    if text_at(held, "strength") != Some("required") {
        return None;
    }
    let url = text_at(held, "valueSet")
        .or_else(|| text_at(held, "valueSetUri"))
        .or_else(|| {
            held.get("valueSetReference")
                .and_then(|reference| text_at(reference, "reference"))
        })?;
    Some(url.split('|').next().unwrap_or(url).to_owned())
}

fn minimum(element: &Value) -> i64 {
    element.get("min").and_then(Value::as_i64).unwrap_or(0)
}

fn maximum(element: &Value) -> i64 {
    match text_at(element, "max") {
        Some("*") => -1,
        Some(text) => text.parse::<i64>().unwrap_or(-1),
        None => -1,
    }
}

fn primitive(name: &str, definition: &Value) -> Value {
    let pattern = definition
        .get("snapshot")
        .and_then(|snapshot| snapshot.get("element"))
        .and_then(Value::as_array)
        .and_then(|elements| {
            elements
                .iter()
                .find(|element| text_at(element, "path") == Some(&format!("{name}.value")))
                .cloned()
        })
        .and_then(|element| {
            element
                .get("type")
                .and_then(Value::as_array)
                .and_then(|items| items.first().cloned())
        })
        .and_then(|held| {
            held.get("extension")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|extension| {
                            text_at(extension, "url")
                                .map(|url| url.ends_with("regex"))
                                .unwrap_or(false)
                        })
                        .and_then(|extension| text_at(extension, "valueString"))
                        .map(str::to_owned)
                })
        });
    let mut held = Map::new();
    held.insert("json".to_owned(), Value::String(shape(name).to_owned()));
    if let Some(pattern) = pattern {
        held.insert("pattern".to_owned(), Value::String(pattern));
    }
    Value::Object(held)
}

fn shape(name: &str) -> &'static str {
    match name {
        "boolean" => "boolean",
        "integer" | "unsignedInt" | "positiveInt" | "decimal" => "number",
        _ => "text",
    }
}

fn codes(bundle: &Value, wanted: &BTreeSet<String>) -> (BTreeMap<String, Value>, BTreeSet<String>) {
    let mut systems: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();
    let mut sets: BTreeMap<String, Value> = BTreeMap::new();
    for entry in bundle
        .get("entry")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
    {
        let Some(resource) = entry.get("resource") else {
            continue;
        };
        let Some(url) = text_at(resource, "url") else {
            continue;
        };
        match text_at(resource, "resourceType") {
            Some("CodeSystem") => {
                let complete = text_at(resource, "content") == Some("complete");
                let listed = complete.then(|| concepts(resource.get("concept")));
                systems.insert(url.to_owned(), listed);
            }
            Some("ValueSet") => {
                sets.insert(url.to_owned(), resource.clone());
            }
            _ => {}
        }
    }
    let mut resolved = BTreeMap::new();
    let mut unresolved = BTreeSet::new();
    for url in wanted {
        match expand(url, &sets, &systems, 0) {
            Some(listed) => {
                resolved.insert(
                    url.clone(),
                    Value::Array(listed.into_iter().map(Value::String).collect()),
                );
            }
            None => {
                unresolved.insert(url.clone());
            }
        }
    }
    (resolved, unresolved)
}

fn concepts(held: Option<&Value>) -> Vec<String> {
    let mut listed = Vec::new();
    let Some(items) = held.and_then(Value::as_array) else {
        return listed;
    };
    for concept in items {
        if let Some(code) = text_at(concept, "code") {
            listed.push(code.to_owned());
        }
        listed.extend(concepts(concept.get("concept")));
    }
    listed
}

fn expand(
    url: &str,
    sets: &BTreeMap<String, Value>,
    systems: &BTreeMap<String, Option<Vec<String>>>,
    depth: usize,
) -> Option<Vec<String>> {
    if depth > 8 {
        return None;
    }
    let held = sets.get(url)?;
    if let Some(contained) = held
        .get("expansion")
        .and_then(|expansion| expansion.get("contains"))
    {
        let listed = contained_codes(contained);
        if !listed.is_empty() {
            return Some(sorted(listed));
        }
    }
    let compose = held.get("compose")?;
    if compose.get("exclude").is_some() {
        return None;
    }
    let includes = compose.get("include").and_then(Value::as_array)?;
    let mut listed = Vec::new();
    for include in includes {
        if include.get("filter").is_some() {
            return None;
        }
        if let Some(nested) = include.get("valueSet").and_then(Value::as_array) {
            for reference in nested.iter().filter_map(Value::as_str) {
                let base = reference.split('|').next().unwrap_or(reference);
                listed.extend(expand(base, sets, systems, depth + 1)?);
            }
            continue;
        }
        if let Some(inline) = include.get("concept") {
            listed.extend(concepts(Some(inline)));
            continue;
        }
        let system = text_at(include, "system")?;
        let base = system.split('|').next().unwrap_or(system);
        listed.extend(systems.get(base)?.clone()?);
    }
    match listed.is_empty() {
        true => None,
        false => Some(sorted(listed)),
    }
}

fn contained_codes(held: &Value) -> Vec<String> {
    let mut listed = Vec::new();
    let Some(items) = held.as_array() else {
        return listed;
    };
    for item in items {
        if let Some(code) = text_at(item, "code") {
            listed.push(code.to_owned());
        }
        if let Some(nested) = item.get("contains") {
            listed.extend(contained_codes(nested));
        }
    }
    listed
}

fn sorted(listed: Vec<String>) -> Vec<String> {
    let mut held: Vec<String> = listed
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    held.sort();
    held
}

fn text_at<'a>(held: &'a Value, name: &str) -> Option<&'a str> {
    held.get(name).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPES: &str = r#"{"resourceType":"Bundle","entry":[
      {"resource":{"resourceType":"StructureDefinition","name":"code","kind":"primitive-type",
        "snapshot":{"element":[{"path":"code.value","type":[{"code":"http://hl7.org/fhirpath/System.String",
          "extension":[{"url":"http://hl7.org/fhir/StructureDefinition/regex","valueString":"[^\\s]+"}]}]}]}}},
      {"resource":{"resourceType":"StructureDefinition","name":"boolean","kind":"primitive-type",
        "snapshot":{"element":[{"path":"boolean.value","type":[{"code":"http://hl7.org/fhirpath/System.Boolean"}]}]}}},
      {"resource":{"resourceType":"StructureDefinition","name":"HumanName","kind":"complex-type",
        "snapshot":{"element":[
          {"path":"HumanName"},
          {"path":"HumanName.family","min":0,"max":"1","type":[{"code":"string"}]},
          {"path":"HumanName.given","min":0,"max":"*","type":[{"code":"string"}]}]}}}]}"#;

    const RESOURCES: &str = r#"{"resourceType":"Bundle","entry":[
      {"resource":{"resourceType":"StructureDefinition","name":"Patient","kind":"resource","url":"http://example.test/StructureDefinition/Patient",
        "abstract":false,"derivation":"specialization",
        "snapshot":{"element":[
          {"path":"Patient"},
          {"path":"Patient.active","min":0,"max":"1","type":[{"code":"boolean"}]},
          {"path":"Patient.name","min":0,"max":"*","type":[{"code":"HumanName"}]},
          {"path":"Patient.gender","min":0,"max":"1","type":[{"code":"code"}],
            "binding":{"strength":"required","valueSet":"http://example.test/ValueSet/gender|4.0.1"}},
          {"path":"Patient.status","min":1,"max":"1","type":[{"code":"code"}],
            "binding":{"strength":"required","valueSet":"http://example.test/ValueSet/open"}},
          {"path":"Patient.contact","min":0,"max":"*","type":[{"code":"BackboneElement"}]},
          {"path":"Patient.contact.name","min":0,"max":"1","type":[{"code":"HumanName"}]}]}}},
      {"resource":{"resourceType":"StructureDefinition","name":"Resource","kind":"resource",
        "abstract":true,"derivation":"specialization","snapshot":{"element":[{"path":"Resource"}]}}}]}"#;

    const SETS: &str = r#"{"resourceType":"Bundle","entry":[
      {"resource":{"resourceType":"CodeSystem","url":"http://example.test/gender","content":"complete",
        "concept":[{"code":"male"},{"code":"female","concept":[{"code":"other"}]}]}},
      {"resource":{"resourceType":"ValueSet","url":"http://example.test/ValueSet/gender",
        "compose":{"include":[{"system":"http://example.test/gender"}]}}},
      {"resource":{"resourceType":"ValueSet","url":"http://example.test/ValueSet/open",
        "compose":{"include":[{"system":"http://example.test/unknown"}]}}}]}"#;

    fn built() -> Value {
        let artifacts = Artifacts::parse(RESOURCES, TYPES, SETS).expect("the artifacts parse");
        let text = definitions(FhirVersion::R4, &artifacts).expect("the definitions are built");
        serde_json::from_str(&text).expect("the definitions are a document")
    }

    #[test]
    fn every_named_resource_type_is_listed_and_abstract_ones_are_not() {
        let file = built();
        assert_eq!(file["resources"], serde_json::json!(["Patient"]));
        assert_eq!(file["version"], "R4");
        assert_eq!(file["release"], "4.0.1");
        assert_eq!(
            file["profiles"]["http://example.test/StructureDefinition/Patient"],
            "Patient"
        );
    }

    #[test]
    fn an_element_carries_its_types_and_cardinality() {
        let file = built();
        let elements = file["nodes"]["Patient"]
            .as_array()
            .expect("patient has elements");
        let name = elements
            .iter()
            .find(|held| held["n"] == "name")
            .expect("name is an element");
        assert_eq!(name["t"], serde_json::json!(["HumanName"]));
        assert_eq!(name["max"], -1);
        let status = elements
            .iter()
            .find(|held| held["n"] == "status")
            .expect("status is an element");
        assert_eq!(status["min"], 1);
        assert_eq!(status["max"], 1);
    }

    #[test]
    fn a_backbone_path_becomes_its_own_node() {
        let file = built();
        let nested = file["nodes"]["Patient.contact"]
            .as_array()
            .expect("the backbone path is a node");
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0]["n"], "name");
    }

    #[test]
    fn a_primitive_carries_its_pattern_and_its_shape() {
        let file = built();
        assert_eq!(file["primitives"]["code"]["json"], "text");
        assert_eq!(file["primitives"]["code"]["pattern"], "[^\\s]+");
        assert_eq!(file["primitives"]["boolean"]["json"], "boolean");
    }

    #[test]
    fn a_required_binding_resolves_to_its_codes_or_is_named_unresolved() {
        let file = built();
        assert_eq!(
            file["bindings"]["http://example.test/ValueSet/gender"],
            serde_json::json!(["female", "male", "other"])
        );
        assert_eq!(
            file["unresolved"],
            serde_json::json!(["http://example.test/ValueSet/open"])
        );
    }

    #[test]
    fn the_same_artifacts_produce_the_same_bytes() {
        let artifacts = Artifacts::parse(RESOURCES, TYPES, SETS).expect("the artifacts parse");
        let first = definitions(FhirVersion::R4, &artifacts).expect("built once");
        let second = definitions(FhirVersion::R4, &artifacts).expect("built twice");
        assert_eq!(first, second);
    }

    #[test]
    fn artifacts_that_name_no_resource_are_refused() {
        let artifacts = Artifacts::parse(r#"{"resourceType":"Bundle"}"#, TYPES, SETS).unwrap();
        assert!(definitions(FhirVersion::R4, &artifacts).is_err());
    }
}
