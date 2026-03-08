
















use crate::search::value::ValueType;
use crate::FhirVersion;
use serde_json::Value;
use std::collections::BTreeMap;

const STU3: &str = include_str!("../../search-parameters/stu3.json");
const R4: &str = include_str!("../../search-parameters/r4.json");
const R4B: &str = include_str!("../../search-parameters/r4b.json");
const R5: &str = include_str!("../../search-parameters/r5.json");

fn generated(version: FhirVersion) -> &'static str {
    match version {
        FhirVersion::Stu3 => STU3,
        FhirVersion::R4 => R4,
        FhirVersion::R4b => R4B,
        FhirVersion::R5 => R5,
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub kind: String,
    pub name: String,
    pub value_type: ValueType,
    pub paths: Vec<String>,
    pub targets: Vec<String>,
    pub since: FhirVersion,
    pub until: Option<FhirVersion>,
}

fn value_type(named: &str) -> Option<ValueType> {
    match named {
        "number" => Some(ValueType::Number),
        "date" => Some(ValueType::Date),
        "string" => Some(ValueType::String),
        "token" => Some(ValueType::Token),
        "quantity" => Some(ValueType::Quantity),
        "reference" => Some(ValueType::Reference),
        "uri" => Some(ValueType::Uri),
        _ => None,
    }
}

fn texts(held: &Value) -> Vec<String> {
    held.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}







pub fn all() -> Vec<Published> {
    let mut held: BTreeMap<(String, String), Published> = BTreeMap::new();
    for version in FhirVersion::ALL {
        let Ok(document) = serde_json::from_str::<Value>(generated(version)) else {
            continue;
        };
        for parameter in document["parameters"].as_array().into_iter().flatten() {
            let (Some(kind), Some(name), Some(named)) = (
                parameter["base"].as_str(),
                parameter["name"].as_str(),
                parameter["type"].as_str(),
            ) else {
                continue;
            };
            let Some(value_type) = value_type(named) else {
                continue;
            };
            let paths = texts(&parameter["paths"]);
            if paths.is_empty() {
                continue;
            }
            let key = (kind.to_owned(), name.to_owned());
            match held.get_mut(&key) {
                Some(entry) => {
                    entry.until = Some(version);
                    for path in paths {
                        if !entry.paths.contains(&path) {
                            entry.paths.push(path);
                        }
                    }
                }
                None => {
                    held.insert(
                        key,
                        Published {
                            kind: kind.to_owned(),
                            name: name.to_owned(),
                            value_type,
                            paths,
                            targets: texts(&parameter["targets"]),
                            since: version,
                            until: Some(version),
                        },
                    );
                }
            }
        }
    }
    held.into_values()
        .map(|mut entry| {
            if entry.until == Some(FhirVersion::R5) {
                entry.until = None;
            }
            entry
        })
        
        
        .filter_map(
            |mut entry| match crate::search::errata::corrected(&mut entry) {
                true => Some(entry),
                false => None,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_parameters_cover_far_more_than_the_written_ones() {
        let held = all();
        let types: std::collections::BTreeSet<&str> =
            held.iter().map(|entry| entry.kind.as_str()).collect();
        assert!(
            types.len() > 120,
            "only {} types are published here",
            types.len()
        );
        assert!(held.len() > 1000, "only {} parameters", held.len());
    }

    #[test]
    fn the_three_types_srch_15_named_carry_their_compartment_parameters() {
        let held = all();
        for (kind, name) in [
            ("Condition", "patient"),
            ("Procedure", "patient"),
            ("MedicationRequest", "subject"),
        ] {
            let found = held
                .iter()
                .find(|entry| entry.kind == kind && entry.name == name)
                .unwrap_or_else(|| panic!("{kind}.{name} is published"));
            assert!(!found.paths.is_empty(), "{kind}.{name} has no path");
            assert_eq!(found.value_type, ValueType::Reference);
        }
    }

    #[test]
    fn a_span_closes_only_where_a_release_stopped_publishing() {
        let held = all();
        let gender = held
            .iter()
            .find(|entry| entry.kind == "Patient" && entry.name == "gender")
            .expect("Patient.gender is published");
        assert_eq!(gender.since, FhirVersion::Stu3);
        assert_eq!(gender.until, None);
    }
}
