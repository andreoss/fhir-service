use fhir_core::search::compartment::{definitions_in, VersionedDef};
use fhir_core::FhirVersion;
use serde_json::{Map, Value};
use uuid::Uuid;

pub fn definition_json(def: &VersionedDef, base: &str) -> Value {
    let resources: Vec<Value> = def
        .members
        .iter()
        .map(|member| {
            serde_json::json!({
                "code": member.resource_type,
                "param": member.params,
            })
        })
        .collect();
    serde_json::json!({
        "resourceType": "CompartmentDefinition",
        "id": def.code,
        "url": format!("{base}/CompartmentDefinition/{}", def.code),
        "name": def.code,
        "status": "active",
        "experimental": false,
        "code": def.code,
        "search": true,
        "resource": resources,
    })
}

pub fn definitions_bundle(version: FhirVersion, base: &str, self_url: &str) -> Vec<u8> {
    let held = definitions_in(version);
    let entries: Vec<Value> = held
        .iter()
        .map(|def| {
            serde_json::json!({
                "fullUrl": format!("{base}/CompartmentDefinition/{}", def.code),
                "resource": definition_json(def, base),
                "search": {"mode": "match"},
            })
        })
        .collect();
    let mut bundle = Map::new();
    bundle.insert(
        "resourceType".to_owned(),
        Value::String("Bundle".to_owned()),
    );
    bundle.insert("id".to_owned(), Value::String(Uuid::new_v4().to_string()));
    bundle.insert("type".to_owned(), Value::String("searchset".to_owned()));
    bundle.insert("total".to_owned(), Value::from(entries.len()));
    bundle.insert(
        "link".to_owned(),
        Value::Array(vec![
            serde_json::json!({"relation": "self", "url": self_url}),
        ]),
    );
    bundle.insert("entry".to_owned(), Value::Array(entries));
    serde_json::to_vec(&Value::Object(bundle)).expect("bundle is serializable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::search::compartment::definition_in;

    #[test]
    fn a_definition_carries_its_code_and_members() {
        let def = definition_in(FhirVersion::R4, "Patient").unwrap();
        let value = definition_json(&def, "http://localhost");
        assert_eq!(value["resourceType"], "CompartmentDefinition");
        assert_eq!(value["code"], "Patient");
        assert_eq!(
            value["url"],
            "http://localhost/CompartmentDefinition/Patient"
        );
        assert_eq!(value["resource"][0]["code"], "Patient");
        assert!(value["resource"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["code"] == "Observation")));
    }

    #[test]
    fn every_definition_is_listed_once() {
        let bytes = definitions_bundle(
            FhirVersion::R4,
            "http://localhost",
            "http://localhost/CompartmentDefinition",
        );
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["total"], definitions_in(FhirVersion::R4).len());
        assert_eq!(value["entry"][0]["search"]["mode"], "match");
        assert_eq!(value["link"][0]["relation"], "self");
    }
}
