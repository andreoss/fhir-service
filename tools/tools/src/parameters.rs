














use fhir_core::{Error, FhirVersion};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub base: String,
    pub value_type: String,
    pub paths: Vec<String>,
    pub targets: Vec<String>,
}


#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    pub published: usize,
    pub converted: usize,
    pub composite: usize,
    pub unexpressed: usize,
    pub fhirpath: Vec<String>,
}

fn value_type(named: &str) -> Option<&'static str> {
    match named {
        "number" => Some("number"),
        "date" => Some("date"),
        "string" => Some("string"),
        "token" => Some("token"),
        "quantity" => Some("quantity"),
        "reference" => Some("reference"),
        "uri" => Some("uri"),
        _ => None,
    }
}



fn plain(segment: &str) -> bool {
    let mut parts = segment.split('.');
    let Some(head) = parts.next() else {
        return false;
    };
    if head.is_empty() || !head.chars().next().is_some_and(char::is_uppercase) {
        return false;
    }
    let mut any = false;
    for part in parts {
        any = true;
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
    }
    any && head.chars().all(|c| c.is_ascii_alphanumeric())
}



fn paths_by_type(expression: &str) -> BTreeMap<String, Vec<String>> {
    let mut held: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for segment in expression.split('|').map(str::trim) {
        if !plain(segment) {
            continue;
        }
        let Some((kind, rest)) = segment.split_once('.') else {
            continue;
        };
        held.entry(kind.to_owned())
            .or_default()
            .push(rest.to_owned());
    }
    held
}


pub fn read(directory: &Path, version: FhirVersion) -> Result<(Vec<Parameter>, Tally), Error> {
    let path = directory.join(format!("{}-search-parameters.json", version.as_str()));
    let text = std::fs::read_to_string(&path)
        .map_err(|error| Error::Config(format!("{} cannot be read: {error}", path.display())))?;
    let body: Value =
        serde_json::from_str(&text).map_err(|error| Error::InvalidJson(error.to_string()))?;
    let entries = body
        .get("entry")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Config(format!("{} carries no entry", path.display())))?;

    let mut held: BTreeMap<(String, String), Parameter> = BTreeMap::new();
    let mut tally = Tally::default();
    for entry in entries {
        let resource = &entry["resource"];
        if resource["resourceType"] != "SearchParameter" {
            continue;
        }
        tally.published += 1;
        let Some(name) = resource["code"].as_str() else {
            continue;
        };
        let Some(kind) = resource["type"].as_str() else {
            continue;
        };
        if kind == "composite" {
            tally.composite += 1;
            continue;
        }
        let Some(value_type) = value_type(kind) else {
            tally.unexpressed += 1;
            continue;
        };
        let Some(expression) = resource["expression"].as_str() else {
            tally.unexpressed += 1;
            continue;
        };
        let bases: BTreeSet<String> = resource["base"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let targets: Vec<String> = resource["target"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let by_type = paths_by_type(expression);
        let mut landed = false;
        for base in &bases {
            let Some(paths) = by_type.get(base) else {
                continue;
            };
            landed = true;
            held.insert(
                (base.clone(), name.to_owned()),
                Parameter {
                    name: name.to_owned(),
                    base: base.clone(),
                    value_type: value_type.to_owned(),
                    paths: paths.clone(),
                    targets: targets.clone(),
                },
            );
        }
        match landed {
            true => tally.converted += 1,
            false => tally.fhirpath.push(format!("{name}: {expression}")),
        }
    }
    Ok((held.into_values().collect(), tally))
}



pub fn document(version: FhirVersion, held: &[Parameter], tally: &Tally) -> Value {
    json!({
        "version": version.as_str(),
        "release": version.release(),
        "published": tally.published,
        "converted": tally.converted,
        "composite": tally.composite,
        "unexpressed": tally.unexpressed,
        "fhirpathOnly": tally.fhirpath.len(),
        "types": held.iter().map(|p| p.base.clone()).collect::<BTreeSet<String>>(),
        "parameters": held.iter().map(|p| json!({
            "name": p.name,
            "base": p.base,
            "type": p.value_type,
            "paths": p.paths,
            "targets": p.targets
        })).collect::<Vec<Value>>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_dotted_path_is_told_from_fhirpath() {
        assert!(plain("Patient.name.family"));
        assert!(plain("Observation.subject"));
        assert!(!plain("Patient"), "a bare type names no element");
        assert!(!plain("Patient.name.where(use='official')"));
        assert!(!plain("(Patient.deceased as dateTime)"));
        assert!(!plain("Observation.value.as(Quantity)"));
        assert!(!plain("name.family"), "a path must name its type");
    }

    #[test]
    fn a_union_gives_each_type_the_paths_it_names() {
        let held = paths_by_type("Patient.deceasedDateTime | Patient.deceasedBoolean");
        assert_eq!(
            held.get("Patient"),
            Some(&vec![
                "deceasedDateTime".to_owned(),
                "deceasedBoolean".to_owned()
            ])
        );
        let across = paths_by_type("AllergyIntolerance.recordedDate | CarePlan.period");
        assert_eq!(across.len(), 2);
        assert_eq!(across.get("CarePlan"), Some(&vec!["period".to_owned()]));
        assert!(paths_by_type("Patient.name.where(use='x')").is_empty());
    }

    #[test]
    fn a_composite_is_counted_rather_than_converted() {
        assert!(value_type("composite").is_none());
        assert!(value_type("special").is_none());
        assert_eq!(value_type("reference"), Some("reference"));
    }
}
