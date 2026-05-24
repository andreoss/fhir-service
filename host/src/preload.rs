use fhir_core::validate::{validate, Mode, Request};
use fhir_core::{Error, FhirVersion, ResourceEnvelope};
use fhir_store::ResourceStore;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Loaded {
    pub files: usize,
    pub written: usize,
    pub unchanged: usize,
}

const JSON: &str = "json";
const NDJSON: &str = "ndjson";

pub fn read(directory: &Path, version: FhirVersion) -> Result<Vec<Value>, Error> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
        .map_err(|error| {
            Error::Config(format!(
                "the preload directory {} cannot be read: {error}",
                directory.display()
            ))
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|held| held.to_str())
                .is_some_and(|held| held == JSON || held == NDJSON)
        })
        .collect();
    files.sort();
    let mut held = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).map_err(|error| {
            Error::Config(format!("{} cannot be read: {error}", path.display()))
        })?;
        let named = path.display().to_string();
        match path.extension().and_then(|held| held.to_str()) {
            Some(NDJSON) => {
                for (line, row) in text.lines().enumerate() {
                    if row.trim().is_empty() {
                        continue;
                    }
                    held.push(parsed(row, &format!("{named} line {}", line + 1))?);
                }
            }
            _ => {
                let value = parsed(&text, &named)?;
                match value.get("resourceType").and_then(Value::as_str) {
                    Some("Bundle") => held.extend(entries_of(&value)),
                    _ => held.push(value),
                }
            }
        }
    }
    for resource in &held {
        judge(resource, version)?;
    }
    Ok(held)
}

pub async fn load(
    store: &dyn ResourceStore,
    version: FhirVersion,
    resources: &[Value],
) -> Result<Loaded, Error> {
    let mut held = Loaded {
        files: resources.len(),
        ..Loaded::default()
    };
    for resource in resources {
        let bytes = serde_json::to_vec(resource)
            .map_err(|error| Error::Internal(format!("a preloaded resource: {error}")))?;
        let envelope = ResourceEnvelope::parse_supplied(version, &bytes)?;
        let key = fhir_core::ResourceKey::of(&envelope);
        let before = store.read(&key).await.ok();

        let written = match store.update(envelope.clone(), None).await {
            Ok(written) => written,
            Err(Error::NotFound) => store.create(envelope).await?,
            Err(error) => return Err(error),
        };
        match before.map(|held| held.version_id().clone()) {
            Some(had) if had == *written.version_id() => held.unchanged += 1,
            _ => held.written += 1,
        }
    }
    Ok(held)
}

fn parsed(text: &str, named: &str) -> Result<Value, Error> {
    serde_json::from_str(text)
        .map_err(|error| Error::Config(format!("{named} is not JSON: {error}")))
}

fn entries_of(bundle: &Value) -> Vec<Value> {
    bundle
        .get("entry")
        .and_then(Value::as_array)
        .map(|held| {
            held.iter()
                .filter_map(|entry| entry.get("resource").cloned())
                .collect()
        })
        .unwrap_or_default()
}

fn judge(resource: &Value, version: FhirVersion) -> Result<(), Error> {
    let named = || {
        let kind = resource
            .get("resourceType")
            .and_then(Value::as_str)
            .unwrap_or("a resource");
        let id = resource
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("(no id)");
        format!("{kind}/{id}")
    };
    if resource.get("id").and_then(Value::as_str).is_none() {
        return Err(Error::Config(format!(
            "a preloaded resource carries no id, so it cannot be written twice \
             to the same place: {}",
            named()
        )));
    }
    let report = validate(&Request {
        version,
        resource_type: None,
        id: None,
        profile: None,
        resolved: None,
        mode: Mode::Update,
        unresolved: fhir_core::validate::Unresolved::Reported,
        body: resource,
    });
    if !report.has_errors() {
        return Ok(());
    }
    let said = report
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
    Err(Error::Config(format!(
        "the preloaded {} does not validate: {said}",
        named()
    )))
}
