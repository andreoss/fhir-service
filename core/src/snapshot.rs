use crate::model::Model;
use crate::{Error, FhirVersion};
use serde_json::{json, Map, Value};

pub const DEPTH: usize = 4;

pub fn generate(version: FhirVersion, definition: &Value) -> Result<Value, Error> {
    let object = definition
        .as_object()
        .ok_or_else(|| Error::InvalidEnvelope("a definition is a resource".to_owned()))?;
    if object.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
        return Err(Error::InvalidEnvelope(
            "a snapshot is generated for a StructureDefinition".to_owned(),
        ));
    }
    let base_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidEnvelope("the definition names no type".to_owned()))?;
    let model = Model::of(version);
    if !model.has_resource(base_type) && !model.has_node(base_type) {
        return Err(Error::InvalidResourceType(base_type.to_owned()));
    }
    if let Some(base) = object.get("baseDefinition").and_then(Value::as_str) {
        let named = base.rsplit('/').next().unwrap_or(base);
        if named != base_type {
            return Err(Error::UnsupportedParameter(format!(
                "{base:?} is a profile of a profile, whose base this instance cannot resolve"
            )));
        }
    }
    let mut elements = from_model(model, base_type);
    let differential = object
        .get("differential")
        .and_then(|held| held.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for stated in &differential {
        fold(&mut elements, stated);
    }
    let mut held = object.clone();
    held.insert(
        "snapshot".to_owned(),
        json!({ "element": Value::Array(elements) }),
    );
    Ok(Value::Object(held))
}

fn from_model(model: &Model, node: &str) -> Vec<Value> {
    let mut held = vec![json!({"path": node, "min": 0, "max": "*"})];
    walk(model, node, node, 1, &mut held);
    held
}

fn walk(model: &Model, node: &str, path: &str, depth: usize, held: &mut Vec<Value>) {
    if depth > DEPTH {
        return;
    }
    for name in model.elements(node) {
        let Some(field) = model.field(node, name) else {
            continue;
        };
        let at = format!("{path}.{name}");
        held.push(definition_of(&at, &field));
        let inner = field.type_name().to_owned();
        if model.has_node(&inner) && inner != node {
            walk(model, &inner, &at, depth + 1, held);
        }
    }
}

fn definition_of(path: &str, field: &crate::model::Field) -> Value {
    let mut held = Map::new();
    held.insert("path".to_owned(), Value::String(path.to_owned()));
    held.insert("min".to_owned(), Value::from(u8::from(field.required())));
    held.insert(
        "max".to_owned(),
        Value::String(match field.repeating() {
            true => "*".to_owned(),
            false => "1".to_owned(),
        }),
    );
    held.insert("type".to_owned(), json!([{ "code": field.type_name() }]));
    Value::Object(held)
}

fn fold(elements: &mut Vec<Value>, stated: &Value) {
    let Some(path) = stated.get("path").and_then(Value::as_str) else {
        return;
    };
    let slice = stated.get("sliceName").and_then(Value::as_str);
    let at = elements.iter().position(|held| {
        held.get("path").and_then(Value::as_str) == Some(path)
            && held.get("sliceName").and_then(Value::as_str) == slice
    });
    match at {
        Some(at) => {
            let Some(held) = elements[at].as_object_mut() else {
                return;
            };
            if let Some(object) = stated.as_object() {
                for (name, value) in object {
                    held.insert(name.clone(), value.clone());
                }
            }
        }
        None => {
            let after = elements
                .iter()
                .rposition(|held| held.get("path").and_then(Value::as_str) == Some(path));
            match after {
                Some(at) => elements.insert(at + 1, stated.clone()),
                None => elements.push(stated.clone()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(elements: Value) -> Value {
        json!({
            "resourceType": "StructureDefinition",
            "url": "http://example.test/StructureDefinition/watched",
            "type": "Patient",
            "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
            "differential": {"element": elements}
        })
    }

    fn paths(held: &Value) -> Vec<String> {
        held["snapshot"]["element"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|element| element["path"].as_str().map(str::to_owned))
            .collect()
    }

    fn at<'a>(held: &'a Value, path: &str) -> &'a Value {
        held["snapshot"]["element"]
            .as_array()
            .unwrap()
            .iter()
            .find(|element| element["path"].as_str() == Some(path))
            .unwrap_or_else(|| panic!("{path} is in the snapshot"))
    }

    #[test]
    fn a_snapshot_carries_the_elements_the_release_gives_the_type() {
        let held = generate(FhirVersion::R4, &definition(json!([]))).unwrap();
        let named = paths(&held);
        assert!(named.contains(&"Patient".to_owned()));
        assert!(named.contains(&"Patient.name".to_owned()), "{named:?}");
        assert!(named.contains(&"Patient.identifier".to_owned()));
        assert!(
            named.iter().any(|path| path == "Patient.name.family"),
            "it descends into what an element contains: {named:?}"
        );
    }

    #[test]
    fn what_the_differential_states_is_folded_over_the_base() {
        let held = generate(
            FhirVersion::R4,
            &definition(json!([{"path": "Patient.identifier", "min": 1, "max": "*"}])),
        )
        .unwrap();
        assert_eq!(at(&held, "Patient.identifier")["min"], 1);
        assert_eq!(at(&held, "Patient.name")["min"], 0, "the rest is untouched");
    }

    #[test]
    fn a_slice_is_kept_beside_the_element_it_refines() {
        let held = generate(
            FhirVersion::R4,
            &definition(json!([
                {"path": "Patient.identifier", "slicing": {"discriminator": [{"type": "value", "path": "system"}]}},
                {"path": "Patient.identifier", "sliceName": "national", "min": 1, "max": "1"}
            ])),
        )
        .unwrap();
        let named = paths(&held);
        let first = named.iter().position(|path| path == "Patient.identifier");
        assert!(first.is_some());
        let slices = held["snapshot"]["element"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|element| element["sliceName"].as_str() == Some("national"))
            .count();
        assert_eq!(slices, 1);
    }

    #[test]
    fn a_definition_that_is_not_one_is_refused() {
        assert!(generate(FhirVersion::R4, &json!({"resourceType": "Patient"})).is_err());
    }

    #[test]
    fn a_type_the_release_does_not_publish_is_refused() {
        let mut held = definition(json!([]));
        held["type"] = json!("Nonesuch");
        assert!(generate(FhirVersion::R4, &held).is_err());
    }

    #[test]
    fn a_profile_of_a_profile_is_refused_rather_than_half_built() {
        let mut held = definition(json!([]));
        held["baseDefinition"] = json!("http://example.test/StructureDefinition/another");
        let error = generate(FhirVersion::R4, &held).unwrap_err();
        assert!(
            error.to_string().contains("profile of a profile"),
            "{error}"
        );
    }
}
