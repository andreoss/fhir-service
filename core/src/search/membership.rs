use crate::search::path::select;
use crate::{FhirInstant, ResourceType};
use serde_json::Value;

pub const COLLECTIONS: [&str; 3] = ["CareTeam", "Group", "List"];

pub fn is_collection(kind: &ResourceType) -> bool {
    COLLECTIONS.contains(&kind.as_str())
}

pub fn collection_types() -> Vec<ResourceType> {
    COLLECTIONS
        .iter()
        .filter_map(|name| name.parse::<ResourceType>().ok())
        .collect()
}

pub fn active(kind: &ResourceType, body: &Value, now: &FhirInstant) -> Vec<String> {
    match kind.as_str() {
        "Group" => members(body, "member", "entity.reference", Some("inactive"), now),
        "List" => members(body, "entry", "item.reference", Some("deleted"), now),
        "CareTeam" => members(body, "participant", "member.reference", None, now),
        _ => Vec::new(),
    }
}

pub fn identity(reference: &str) -> &str {
    match reference.rsplit_once('/') {
        Some((_, id)) => id,
        None => reference,
    }
}

fn members(
    body: &Value,
    path: &str,
    reference: &str,
    excluded: Option<&str>,
    now: &FhirInstant,
) -> Vec<String> {
    let mut found = Vec::new();
    for element in elements(body, path) {
        if excluded.is_some_and(|flag| element.get(flag).and_then(Value::as_bool) == Some(true)) {
            continue;
        }
        if !open(element, now) {
            continue;
        }
        for held in select(element, reference) {
            if let Some(text) = held.as_str() {
                let id = identity(text);
                if !id.is_empty() && !found.iter().any(|kept| kept == id) {
                    found.push(id.to_owned());
                }
            }
        }
    }
    found
}

fn elements<'a>(body: &'a Value, path: &str) -> Vec<&'a Value> {
    select(body, path)
        .into_iter()
        .flat_map(|found| match found {
            Value::Array(items) => items.iter().collect::<Vec<&Value>>(),
            other => vec![other],
        })
        .collect()
}

fn open(element: &Value, now: &FhirInstant) -> bool {
    let Some(period) = element.get("period") else {
        return true;
    };
    let begun = bound(period.get("start"), now, true);
    let ended = bound(period.get("end"), now, false);
    begun && ended
}

fn bound(edge: Option<&Value>, now: &FhirInstant, is_start: bool) -> bool {
    let Some(text) = edge.and_then(Value::as_str) else {
        return true;
    };
    let Some(instant) = FhirInstant::parse(text).ok() else {
        return true;
    };
    match is_start {
        true => instant.key() <= now.key(),
        false => instant.key() >= now.key(),
    }
}
