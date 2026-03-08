







use crate::http;
use crate::population::Record;
use fhir_core::{Error, FhirVersion};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const FHIR_JSON: &str = "application/fhir+json";



pub fn predicted(records: &[Record]) -> BTreeMap<String, usize> {
    let mut held = BTreeMap::new();
    for record in records.iter().filter(|record| !record.deleted()) {
        *held.entry(record.resource_type.clone()).or_insert(0) += 1;
    }
    held
}


pub fn observed(
    address: &str,
    host: &str,
    types: impl IntoIterator<Item = String>,
) -> Result<BTreeMap<String, usize>, Error> {
    let mut held = BTreeMap::new();
    for named in types {
        let path = format!("/{named}?_summary=count");
        let (status, _, body) = http::send_typed(address, host, "GET", &path, FHIR_JSON, "")?;
        if status != 200 {
            return Err(Error::Internal(format!(
                "a count of {named} was answered {status}: {body}"
            )));
        }
        let bundle: Value =
            serde_json::from_str(&body).map_err(|e| Error::InvalidJson(e.to_string()))?;
        let total = bundle["total"].as_u64().ok_or_else(|| {
            Error::Internal(format!("a count of {named} carried no total: {body}"))
        })?;
        held.insert(named, total as usize);
    }
    Ok(held)
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub resource_type: String,
    pub predicted: usize,
    pub observed: usize,
}




pub fn differences(
    predicted: &BTreeMap<String, usize>,
    observed: &BTreeMap<String, usize>,
) -> Vec<Difference> {
    let mut held = Vec::new();
    let named: BTreeSet<&String> = predicted.keys().chain(observed.keys()).collect();
    for resource_type in named {
        let left = predicted.get(resource_type).copied().unwrap_or_default();
        let right = observed.get(resource_type).copied().unwrap_or_default();
        if left != right {
            held.push(Difference {
                resource_type: resource_type.clone(),
                predicted: left,
                observed: right,
            });
        }
    }
    held
}




pub fn validated(address: &str, host: &str, records: &[Record]) -> Result<Vec<String>, Error> {
    let mut refused = Vec::new();
    for record in records.iter().filter(|record| !record.deleted()) {
        let Some(body) = record.current() else {
            continue;
        };
        let path = format!("/{}/$validate", record.resource_type);
        let (status, _, answered) =
            http::send_typed(address, host, "POST", &path, FHIR_JSON, &body.to_string())?;
        let outcome: Value = serde_json::from_str(&answered).unwrap_or(Value::Null);
        let errored = outcome["issue"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|issue| matches!(issue["severity"].as_str(), Some("error" | "fatal")));
        if status >= 400 || errored {
            refused.push(format!(
                "{}/{} answered {status}: {answered}",
                record.resource_type, record.id
            ));
        }
    }
    Ok(refused)
}


pub fn everything(address: &str, host: &str, patient: &str) -> Result<BTreeSet<String>, Error> {
    let path = format!("/Patient/{patient}/$everything");
    let (status, _, body) = http::send_typed(address, host, "GET", &path, FHIR_JSON, "")?;
    if status != 200 {
        return Err(Error::Internal(format!(
            "$everything for {patient} was answered {status}: {body}"
        )));
    }
    let bundle: Value =
        serde_json::from_str(&body).map_err(|e| Error::InvalidJson(e.to_string()))?;
    Ok(bundle["entry"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let resource = &entry["resource"];
            let named = resource["resourceType"].as_str()?;
            let id = resource["id"].as_str()?;
            Some(format!("{named}/{id}"))
        })
        .collect())
}







pub fn built(records: &[Record], patient: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    use fhir_core::search::{compartment::definition, lookup};
    use fhir_core::ResourceType;

    let mut expected = BTreeSet::new();
    let mut undecidable = BTreeSet::new();
    let def = definition("Patient");
    for record in records.iter().filter(|record| !record.deleted()) {
        let Ok(resource_type) = record.resource_type.parse::<ResourceType>() else {
            continue;
        };
        let resolvable = def
            .and_then(|def| def.member(resource_type))
            .is_some_and(|member| {
                member
                    .params
                    .iter()
                    .any(|name| lookup(Some(resource_type), name).is_some())
            });
        let named = format!("{}/{}", record.resource_type, record.id);
        match resolvable || record.resource_type == "Patient" {
            true => {
                expected.insert(named);
            }
            false => {
                undecidable.insert(record.resource_type.clone());
            }
        }
    }
    let _ = patient;
    (expected, undecidable)
}


#[derive(Debug, Clone)]
pub struct Figures {
    pub version: FhirVersion,
    pub backend: String,
    pub subjects: u64,
    pub seed: u64,
    pub records: usize,
    pub versions: usize,
    pub by_type: BTreeMap<String, usize>,
    pub load_seconds: f64,
    pub check_seconds: f64,
    pub refused: usize,
    pub differences: Vec<Difference>,
}

impl Figures {
    pub fn to_json(&self) -> Value {
        json!({
            "version": self.version.as_str(),
            "backend": self.backend,
            "subjects": self.subjects,
            "seed": self.seed,
            "records": self.records,
            "versions": self.versions,
            "byType": self.by_type,
            "loadSeconds": (self.load_seconds * 1000.0).round() / 1000.0,
            "checkSeconds": (self.check_seconds * 1000.0).round() / 1000.0,
            "refused": self.refused,
            "differences": self.differences.iter().map(|d| json!({
                "resourceType": d.resource_type,
                "predicted": d.predicted,
                "observed": d.observed
            })).collect::<Vec<Value>>()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::population::history;

    #[test]
    fn a_prediction_counts_what_survives_rather_than_what_was_written() {
        let records = history(FhirVersion::R4, 307, 0);
        let held = predicted(&records);
        let live = records.iter().filter(|record| !record.deleted()).count();
        assert_eq!(held.values().sum::<usize>(), live);
        assert!(held.contains_key("Patient"));
        for record in records.iter().filter(|record| record.deleted()) {
            let counted = held.get(&record.resource_type).copied().unwrap_or_default();
            let all = records
                .iter()
                .filter(|other| other.resource_type == record.resource_type)
                .count();
            assert!(
                counted < all,
                "a deleted {} was counted",
                record.resource_type
            );
        }
    }

    #[test]
    fn a_difference_names_the_type_and_both_numbers() {
        let left = BTreeMap::from([("Patient".to_owned(), 3), ("Observation".to_owned(), 7)]);
        let right = BTreeMap::from([("Patient".to_owned(), 3), ("Observation".to_owned(), 5)]);
        let held = differences(&left, &right);
        assert_eq!(
            held,
            vec![Difference {
                resource_type: "Observation".to_owned(),
                predicted: 7,
                observed: 5
            }]
        );
        assert!(differences(&left, &left).is_empty());
        let missing = BTreeMap::from([("Patient".to_owned(), 3)]);
        assert_eq!(differences(&left, &missing).len(), 1);
    }

    #[test]
    fn what_the_compartment_cannot_decide_is_reported_rather_than_dropped() {
        let records = history(FhirVersion::R4, 311, 0);
        let (expected, undecidable) = built(&records, "s311-000000-p");
        assert!(expected.iter().any(|named| named.starts_with("Patient/")));
        assert!(
            undecidable.is_empty(),
            "these types still cannot be decided: {undecidable:?}"
        );
        for record in records.iter().filter(|record| !record.deleted()) {
            assert!(
                expected
                    .iter()
                    .any(|held| held == &format!("{}/{}", record.resource_type, record.id)),
                "{} is not in the compartment the generator built",
                record.resource_type
            );
        }
    }
}
