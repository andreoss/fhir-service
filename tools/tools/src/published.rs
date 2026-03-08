








use crate::http;
use crate::upload;
use fhir_core::validate::{validate, Mode, Request};
use fhir_core::{Error, FhirVersion, ResourceType};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

const NDJSON_SUFFIX: &str = ".ndjson";




const LOG: &str = "log.ndjson";



#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Published {
    pub rows: BTreeMap<ResourceType, Vec<String>>,
}

impl Published {
    pub fn total(&self) -> usize {
        self.rows.values().map(Vec::len).sum()
    }

    pub fn types(&self) -> Vec<ResourceType> {
        self.rows.keys().copied().collect()
    }

    pub fn counts(&self) -> BTreeMap<ResourceType, usize> {
        self.rows
            .iter()
            .map(|(kind, held)| (*kind, held.len()))
            .collect()
    }

    
    pub fn ndjson(&self) -> String {
        let mut held = String::new();
        for rows in self.rows.values() {
            for row in rows {
                held.push_str(row);
                held.push('\n');
            }
        }
        held
    }

    
    
    
    pub fn chunks(&self, bytes: usize) -> Vec<String> {
        let mut held = Vec::new();
        let mut current = String::new();
        for rows in self.rows.values() {
            for row in rows {
                if !current.is_empty() && current.len() + row.len() + 1 > bytes {
                    held.push(std::mem::take(&mut current));
                }
                current.push_str(row);
                current.push('\n');
            }
        }
        if !current.is_empty() {
            held.push(current);
        }
        held
    }
}




pub fn read(directory: &Path, version: FhirVersion) -> Result<Published, Error> {
    let listed = std::fs::read_dir(directory)
        .map_err(|error| Error::Config(format!("{}: {error}", directory.display())))?;
    let mut held = Published::default();
    for entry in listed {
        let entry = entry.map_err(|error| Error::Config(error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(NDJSON_SUFFIX) || name == LOG {
            continue;
        }
        let base = name.split('.').next().unwrap_or_default();
        let kind = base.parse::<ResourceType>().map_err(|_| {
            Error::Config(format!("{name} names {base:?}, which is no resource type"))
        })?;
        if !ResourceType::served(version).contains(&kind) {
            return Err(Error::Config(format!(
                "{name} names {kind}, which {version} does not serve"
            )));
        }
        let body = std::fs::read_to_string(entry.path())
            .map_err(|error| Error::Config(format!("{name}: {error}")))?;
        let rows: Vec<String> = body
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        held.rows.entry(kind).or_default().extend(rows);
    }
    if held.rows.is_empty() {
        return Err(Error::Config(format!(
            "{} holds no ndjson of any served type",
            directory.display()
        )));
    }
    Ok(held)
}



#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub resource_type: ResourceType,
    pub id: String,
    pub issue: String,
}



pub fn judged(held: &Published, version: FhirVersion) -> Result<Vec<Refused>, Error> {
    let mut refused = Vec::new();
    for (kind, rows) in &held.rows {
        for row in rows {
            let body: Value = serde_json::from_str(row)
                .map_err(|error| Error::InvalidJson(format!("{kind}: {error}")))?;
            let report = validate(&Request {
                version,
                resource_type: Some(*kind),
                id: None,
                profile: None,
                resolved: None,
                mode: Mode::Update,
                body: &body,
            });
            if !report.has_errors() {
                continue;
            }
            let issue = report
                .issues()
                .iter()
                .find(|issue| {
                    matches!(
                        issue.severity,
                        fhir_core::IssueSeverity::Error | fhir_core::IssueSeverity::Fatal
                    )
                })
                .map(|issue| issue.diagnostics.clone())
                .unwrap_or_default();
            refused.push(Refused {
                resource_type: *kind,
                id: body
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("(no id)")
                    .to_owned(),
                issue,
            });
        }
    }
    Ok(refused)
}


pub fn held(
    address: &str,
    host: &str,
    types: &[ResourceType],
) -> Result<BTreeMap<ResourceType, usize>, Error> {
    let mut counts = BTreeMap::new();
    for kind in types {
        let path = format!("/{}?_summary=count&_total=accurate", kind.as_str());
        let (status, _, body) = http::send_typed(address, host, "GET", &path, "", "")?;
        if status != 200 {
            return Err(Error::Internal(format!(
                "{kind} was counted with {status}: {body}"
            )));
        }
        let bundle: Value = serde_json::from_str(&body)
            .map_err(|error| Error::InvalidJson(format!("{kind}: {error}")))?;
        let total = bundle
            .get("total")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::Internal(format!("{kind} answered no total: {body}")))?;
        counts.insert(*kind, total as usize);
    }
    Ok(counts)
}



pub const CHUNK_BYTES: usize = 512 * 1024;



pub fn load(
    address: &str,
    host: &str,
    held: &Published,
    patience: Duration,
) -> Result<upload::Loaded, Error> {
    let mut total = upload::Loaded::default();
    for rows in held.chunks(CHUNK_BYTES) {
        let loaded = upload::import(address, host, &rows, patience)?;
        total.submitted += loaded.submitted;
        total.written += loaded.written;
        total.unchanged += loaded.unchanged;
        total.failures += loaded.failures;
    }
    Ok(total)
}




const PER_BUNDLE: usize = 250;








pub fn load_through_bundles(
    address: &str,
    host: &str,
    held: &Published,
) -> Result<upload::Loaded, Error> {
    let mut total = upload::Loaded::default();
    let mut entries: Vec<Value> = Vec::new();
    let mut send = |entries: &mut Vec<Value>| -> Result<(), Error> {
        if entries.is_empty() {
            return Ok(());
        }
        let bundle = serde_json::json!({
            "resourceType": "Bundle",
            "type": "transaction",
            "entry": std::mem::take(entries),
        });
        let loaded = upload::transact(address, host, &bundle)?;
        total.submitted += loaded.submitted;
        total.written += loaded.written;
        total.unchanged += loaded.unchanged;
        total.failures += loaded.failures;
        Ok(())
    };
    for (kind, rows) in &held.rows {
        for row in rows {
            let resource: Value =
                serde_json::from_str(row).map_err(|error| Error::InvalidJson(error.to_string()))?;
            let id = resource["id"].as_str().ok_or_else(|| {
                Error::InvalidEnvelope(format!("a published {kind} carries no id"))
            })?;
            entries.push(serde_json::json!({
                "resource": resource,
                "request": {"method": "PUT", "url": format!("{kind}/{id}")},
            }));
            if entries.len() >= PER_BUNDLE {
                send(&mut entries)?;
            }
        }
    }
    send(&mut entries)?;
    Ok(total)
}



#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
    pub resource_type: ResourceType,
    pub published: usize,
    pub held: usize,
}

impl Figure {
    pub fn agrees(&self) -> bool {
        self.published == self.held
    }
}

pub fn figures(
    published: &BTreeMap<ResourceType, usize>,
    held: &BTreeMap<ResourceType, usize>,
) -> Vec<Figure> {
    published
        .iter()
        .map(|(kind, count)| Figure {
            resource_type: *kind,
            published: *count,
            held: held.get(kind).copied().unwrap_or_default(),
        })
        .collect()
}



pub fn beyond(published: &[ResourceType], generated: &[ResourceType]) -> Vec<ResourceType> {
    published
        .iter()
        .filter(|kind| !generated.contains(kind))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a served type")
    }

    fn set() -> Published {
        let mut held = Published::default();
        held.rows.insert(
            kind("Patient"),
            vec![
                r#"{"resourceType":"Patient","id":"a"}"#.to_owned(),
                r#"{"resourceType":"Patient","id":"b"}"#.to_owned(),
            ],
        );
        held.rows.insert(
            kind("Observation"),
            vec![
                r#"{"resourceType":"Observation","id":"c","status":"final","code":{"text":"x"}}"#
                    .to_owned(),
            ],
        );
        held
    }

    #[test]
    fn a_set_counts_what_it_holds() {
        let held = set();
        assert_eq!(held.total(), 3);
        assert_eq!(held.counts()[&kind("Patient")], 2);
        assert_eq!(held.types().len(), 2);
    }

    #[test]
    fn the_rows_become_one_ndjson_body() {
        let body = set().ndjson();
        assert_eq!(body.lines().count(), 3);
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn a_set_larger_than_one_request_is_sent_as_several() {
        let held = set();
        assert_eq!(held.chunks(1_000_000).len(), 1);
        let split = held.chunks(60);
        assert!(split.len() > 1, "{split:?}");
        assert_eq!(
            split.iter().map(|rows| rows.lines().count()).sum::<usize>(),
            held.total(),
            "no row is lost between the requests"
        );
    }

    #[test]
    fn a_row_longer_than_the_bound_is_still_sent_whole() {
        let held = set();
        let split = held.chunks(1);
        assert_eq!(split.len(), held.total());
        for rows in &split {
            assert_eq!(rows.lines().count(), 1);
        }
    }

    #[test]
    fn every_row_of_a_well_formed_set_is_accepted() {
        assert!(judged(&set(), FhirVersion::R4).unwrap().is_empty());
    }

    #[test]
    fn a_row_the_release_refuses_names_itself_and_its_issue() {
        let mut held = set();
        held.rows.insert(
            kind("Observation"),
            vec![r#"{"resourceType":"Observation","id":"bad","status":5}"#.to_owned()],
        );
        let refused = judged(&held, FhirVersion::R4).unwrap();
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].id, "bad");
        assert!(!refused[0].issue.is_empty());
    }

    #[test]
    fn a_figure_says_whether_the_two_counts_agree() {
        let published = set().counts();
        let mut held = published.clone();
        assert!(figures(&published, &held).iter().all(Figure::agrees));
        held.insert(kind("Patient"), 1);
        let figures = figures(&published, &held);
        let patient = figures
            .iter()
            .find(|figure| figure.resource_type == kind("Patient"))
            .unwrap();
        assert!(!patient.agrees());
        assert_eq!(patient.published, 2);
        assert_eq!(patient.held, 1);
    }

    #[test]
    fn the_difference_against_the_generator_is_the_types_it_does_not_write() {
        let published = vec![kind("Patient"), kind("Specimen"), kind("Device")];
        let generated = vec![kind("Patient"), kind("Observation")];
        assert_eq!(
            beyond(&published, &generated),
            vec![kind("Specimen"), kind("Device")]
        );
    }

    #[test]
    fn a_directory_that_is_not_there_is_refused() {
        let error = read(Path::new("/nowhere-at-all"), FhirVersion::R4).unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{error:?}");
    }
}
