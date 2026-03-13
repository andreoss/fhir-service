use crate::http;
use crate::population::Record;
use fhir_core::Error;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const POLL_EVERY: Duration = Duration::from_millis(200);
const NDJSON: &str = "application/fhir+ndjson";
const FHIR_JSON: &str = "application/fhir+json";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    pub submitted: usize,
    pub written: usize,
    pub unchanged: usize,
    pub failures: usize,
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn path_of(location: &str) -> String {
    match location.find("://") {
        None => location.to_owned(),
        Some(start) => match location[start + 3..].find('/') {
            None => "/".to_owned(),
            Some(rest) => location[start + 3 + rest..].to_owned(),
        },
    }
}

pub fn import(address: &str, host: &str, rows: &str, patience: Duration) -> Result<Loaded, Error> {
    let (status, headers, body) =
        http::send_typed(address, host, "POST", "/$import", NDJSON, rows)?;
    if status != 202 {
        return Err(Error::Internal(format!(
            "the import was answered {status}: {body}"
        )));
    }
    let location = header(&headers, "content-location").ok_or_else(|| {
        Error::Internal("the import was accepted without a status location".to_owned())
    })?;
    let path = path_of(location);
    let submitted = rows.lines().filter(|line| !line.trim().is_empty()).count();
    let started = Instant::now();
    let mut wait = retry_after(&headers).unwrap_or(POLL_EVERY);
    loop {
        std::thread::sleep(wait);
        let (status, headers, body) = http::send_typed(address, host, "GET", &path, FHIR_JSON, "")?;
        wait = retry_after(&headers).unwrap_or(POLL_EVERY);
        match status {
            202 | 429 => {}
            200 => return Ok(counted(submitted, &body)),
            other => {
                return Err(Error::Internal(format!(
                    "the import status was answered {other}: {body}"
                )))
            }
        }
        if started.elapsed() > patience {
            return Err(Error::Internal(format!(
                "the import did not finish within {patience:?}"
            )));
        }
    }
}

fn retry_after(headers: &[(String, String)]) -> Option<Duration> {
    header(headers, "retry-after")?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn counted(submitted: usize, body: &str) -> Loaded {
    let held: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let outcome = &held["outcome"];
    let number = |name: &str| outcome[name].as_u64().unwrap_or_default() as usize;
    Loaded {
        submitted,
        written: number("handled"),
        unchanged: number("unchanged"),
        failures: outcome["failures"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
    }
}

pub fn transactions(records: &[Record]) -> Vec<Value> {
    let mut writes = Vec::new();
    let mut removals = Vec::new();
    for record in records {
        let url = format!("{}/{}", record.resource_type, record.id);
        for change in &record.changes {
            match change.deleted {
                true => removals.push(json!({
                    "fullUrl": url,
                    "request": { "method": "DELETE", "url": url }
                })),
                false => writes.push(json!({
                    "fullUrl": url,
                    "resource": change.body,
                    "request": { "method": "PUT", "url": url }
                })),
            }
        }
    }
    let bundle = |entry: Vec<Value>| {
        json!({
            "resourceType": "Bundle",
            "type": "transaction",
            "entry": entry
        })
    };
    let mut held = Vec::new();
    if !writes.is_empty() {
        held.push(bundle(writes));
    }
    if !removals.is_empty() {
        held.push(bundle(removals));
    }
    held
}

pub fn remove(address: &str, host: &str, records: &[Record]) -> Result<usize, Error> {
    let mut removed = 0;
    for record in records.iter().filter(|record| record.deleted()) {
        let path = format!("/{}/{}", record.resource_type, record.id);
        let (status, _, body) = http::send_typed(address, host, "DELETE", &path, FHIR_JSON, "")?;
        match status {
            200 | 204 | 404 | 410 => removed += 1,
            other => {
                return Err(Error::Internal(format!(
                    "deleting {path} was answered {other}: {body}"
                )))
            }
        }
    }
    Ok(removed)
}

pub fn load(
    address: &str,
    host: &str,
    records: &[Record],
    patience: std::time::Duration,
) -> Result<Loaded, Error> {
    let outstanding = pending(address, host, records)?;
    if outstanding.is_empty() {
        return Ok(Loaded {
            submitted: 0,
            written: 0,
            unchanged: records.len(),
            failures: 0,
        });
    }
    let rows = rows(&outstanding);
    let mut loaded = match rows.is_empty() {
        true => Loaded::default(),
        false => import(address, host, &rows, patience)?,
    };
    loaded.unchanged = records.len() - outstanding.len();
    remove(address, host, &outstanding)?;
    Ok(loaded)
}

pub fn pending(address: &str, host: &str, records: &[Record]) -> Result<Vec<Record>, Error> {
    let mut held = Vec::new();
    for record in records {
        let path = format!("/{}/{}", record.resource_type, record.id);
        let (status, _, body) = http::send_typed(address, host, "GET", &path, FHIR_JSON, "")?;
        let settled = match status {
            410 => record.deleted(),
            200 => !record.deleted() && matches(&body, record),
            _ => false,
        };
        if !settled {
            held.push(record.clone());
        }
    }
    Ok(held)
}

fn matches(body: &str, record: &Record) -> bool {
    let Some(wanted) = record.current() else {
        return false;
    };
    let Ok(held) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    stripped(&held) == stripped(wanted)
}

fn stripped(body: &Value) -> Value {
    let mut next = body.clone();
    if let Some(object) = next.as_object_mut() {
        object.remove("meta");
    }
    next
}

pub fn transact(address: &str, host: &str, bundle: &Value) -> Result<Loaded, Error> {
    let body = bundle.to_string();
    let (status, _, answered) = http::send_typed(address, host, "POST", "/", FHIR_JSON, &body)?;
    if status != 200 && status != 201 {
        return Err(Error::Internal(format!(
            "the transaction was answered {status}: {answered}"
        )));
    }
    let held: Value =
        serde_json::from_str(&answered).map_err(|e| Error::InvalidJson(e.to_string()))?;
    let entries = held["entry"].as_array().map(Vec::len).unwrap_or_default();
    let failures = held["entry"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["response"]["status"]
                .as_str()
                .map(|status| !status.starts_with('2'))
                .unwrap_or(true)
        })
        .count();
    Ok(Loaded {
        submitted: entries,
        written: entries - failures,
        unchanged: 0,
        failures,
    })
}

pub fn rows(records: &[Record]) -> String {
    let mut text = String::new();
    for record in records {
        for change in &record.changes {
            if change.deleted {
                continue;
            }
            text.push_str(&change.body.to_string());
            text.push('\n');
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::population::history;
    use fhir_core::FhirVersion;

    #[test]
    fn a_status_location_becomes_a_path_to_poll() {
        assert_eq!(path_of("http://localhost:8080/_jobs/x1"), "/_jobs/x1");
        assert_eq!(path_of("https://host/_jobs/x1?a=b"), "/_jobs/x1?a=b");
        assert_eq!(path_of("/_jobs/x1"), "/_jobs/x1");
        assert_eq!(path_of("http://localhost"), "/");
    }

    #[test]
    fn a_transaction_names_a_method_and_url_on_every_entry() {
        let records = history(FhirVersion::R4, 101, 0);
        let bundles = transactions(&records);
        assert!(!bundles.is_empty());
        for bundle in &bundles {
            assert_eq!(bundle["type"], "transaction");
        }
        for bundle in &bundles {
            let methods: Vec<&str> = bundle["entry"]
                .as_array()
                .expect("entries")
                .iter()
                .map(|e| e["request"]["method"].as_str().unwrap_or("?"))
                .collect();
            assert!(
                methods.iter().all(|m| *m == methods[0]),
                "a bundle mixes methods: {methods:?}"
            );
        }
        for entry in bundles
            .iter()
            .flat_map(|b| b["entry"].as_array().expect("entries"))
        {
            let method = entry["request"]["method"].as_str().expect("a method");
            let url = entry["request"]["url"].as_str().expect("a url");
            assert!(
                method == "PUT" || method == "DELETE",
                "a seeded entry is an upsert or a removal, not {method}"
            );
            assert!(url.contains('/'), "the url names a type and an id: {url}");
            assert_eq!(entry["resource"].is_null(), method == "DELETE");
        }
    }

    #[test]
    fn the_rows_carry_every_written_version_and_no_delete_marker() {
        let records = history(FhirVersion::R4, 103, 0);
        let written: usize = records
            .iter()
            .map(|record| record.changes.iter().filter(|c| !c.deleted).count())
            .sum();
        assert_eq!(rows(&records).lines().count(), written);
    }

    #[test]
    fn a_closed_address_is_reported_rather_than_waited_on() {
        let outcome = import("127.0.0.1:1", "localhost", "{}\n", Duration::from_secs(1));
        assert!(outcome.is_err());
    }
}
