use crate::Config;
use fhir_core::{Catalogue, Error};
use serde_json::Value;
use std::path::Path;

pub fn loaded(config: &Config) -> Result<Catalogue, Error> {
    let published = Catalogue::of(config.version).clone();
    let Some(directory) = &config.terminology_dir else {
        return Ok(published);
    };
    let mut held = published;
    for body in read(directory)? {
        held = held.loaded(&body)?;
    }
    Ok(held)
}

fn read(directory: &Path) -> Result<Vec<Value>, Error> {
    let listed = std::fs::read_dir(directory).map_err(|_| {
        Error::Config("the configured terminology directory cannot be read".to_owned())
    })?;
    let mut names: Vec<std::path::PathBuf> = listed
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|held| held == "json"))
        .collect();
    names.sort();
    let mut bodies = Vec::new();
    for path in names {
        let text = std::fs::read_to_string(&path)
            .map_err(|_| Error::Config("a supplied terminology file cannot be read".to_owned()))?;
        let body: Value =
            serde_json::from_str(&text).map_err(|reason| Error::InvalidJson(reason.to_string()))?;
        match body.get("resourceType").and_then(Value::as_str) {
            Some("Bundle") => bodies.extend(entries(&body)),
            _ => bodies.push(body),
        }
    }
    Ok(bodies)
}

fn entries(bundle: &Value) -> Vec<Value> {
    bundle
        .get("entry")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(|entry| entry.get("resource").cloned())
        .collect()
}
