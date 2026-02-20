
use crate::search::select;
use crate::Error;
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Designation {
    pub language: Option<String>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Coding {
    pub system: Option<String>,
    pub version: Option<String>,
    pub code: String,
    pub display: Option<String>,
    pub designations: Vec<Designation>,
    pub inactive: bool,
    pub contains: Vec<Coding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExpansionRequest {
    pub filter: Option<String>,
    pub count: Option<usize>,
    pub offset: usize,
    pub date: Option<String>,
    pub active_only: bool,
    pub display_language: Option<String>,
    pub designations: bool,
    pub exclude_nested: bool,
    pub system_versions: Vec<(String, String)>,
    pub value_set_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stamp {
    pub identifier: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Expansion {
    pub id: Option<String>,
    pub url: Option<String>,
    pub version: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub total: usize,
    pub offset: usize,
    pub concepts: Vec<Coding>,
}

pub fn concepts(code_system: &Value) -> Vec<Coding> {
    let system = code_system.get("url").and_then(Value::as_str);
    let version = code_system.get("version").and_then(Value::as_str);
    branch(code_system.get("concept"), system, version)
}

fn branch(held: Option<&Value>, system: Option<&str>, version: Option<&str>) -> Vec<Coding> {
    let Some(Value::Array(items)) = held else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| coding(item, system, version))
        .collect()
}

fn coding(item: &Value, system: Option<&str>, version: Option<&str>) -> Option<Coding> {
    let code = item.get("code").and_then(Value::as_str)?;
    Some(Coding {
        system: system.map(str::to_owned),
        version: version.map(str::to_owned),
        code: code.to_owned(),
        display: item
            .get("display")
            .and_then(Value::as_str)
            .map(str::to_owned),
        designations: designations(item),
        inactive: inactive(item),
        contains: branch(item.get("concept"), system, version),
    })
}

fn designations(item: &Value) -> Vec<Designation> {
    item.get("designation")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|held| {
                    Some(Designation {
                        language: held
                            .get("language")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        value: held.get("value").and_then(Value::as_str)?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn inactive(item: &Value) -> bool {
    if item.get("inactive").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    select(item, "property")
        .into_iter()
        .flat_map(|found| match found {
            Value::Array(items) => items.iter().collect::<Vec<&Value>>(),
            other => vec![other],
        })
        .any(|property| {
            let code = property.get("code").and_then(Value::as_str);
            let value = property
                .get("valueCode")
                .or_else(|| property.get("valueString"))
                .and_then(Value::as_str);
            let flag = property.get("valueBoolean").and_then(Value::as_bool);
            matches!(
                (code, value, flag),
                (Some("status"), Some("retired" | "deprecated"), _) | (Some("inactive"), _, Some(true))
            )
        })
}

pub fn descendants(code_system: &Value, code: &str) -> Vec<Coding> {
    let held = concepts(code_system);
    match subtree(&held, code) {
        Some(found) => flattened(std::slice::from_ref(found)),
        None => Vec::new(),
    }
}

pub fn ancestors(code_system: &Value, code: &str) -> Vec<Coding> {
    let held = concepts(code_system);
    let mut line = Vec::new();
    if !lineage(&held, code, &mut line) {
        return Vec::new();
    }
    line
}

fn lineage(held: &[Coding], code: &str, out: &mut Vec<Coding>) -> bool {
    for concept in held {
        if concept.code == code {
            out.push(flat(concept));
            return true;
        }
        if lineage(&concept.contains, code, out) {
            out.push(flat(concept));
            return true;
        }
    }
    false
}

pub fn subtree<'a>(held: &'a [Coding], code: &str) -> Option<&'a Coding> {
    for concept in held {
        if concept.code == code {
            return Some(concept);
        }
        if let Some(found) = subtree(&concept.contains, code) {
            return Some(found);
        }
    }
    None
}

pub fn flattened(held: &[Coding]) -> Vec<Coding> {
    let mut out = Vec::new();
    for concept in held {
        out.push(flat(concept));
        out.extend(flattened(&concept.contains));
    }
    out
}

fn flat(concept: &Coding) -> Coding {
    Coding {
        contains: Vec::new(),
        ..concept.clone()
    }
}

pub fn expand(
    set: &Value,
    systems: &[Value],
    request: &ExpansionRequest,
) -> Result<Expansion, Error> {
    versioned(set, request)?;
    let mut held = pre_expanded(set);
    if held.is_empty() {
        held = composed(set, systems, request)?;
    }
    if request.active_only {
        held = kept(&held, &|concept: &Coding| !concept.inactive);
    }
    if let Some(filter) = &request.filter {
        let wanted = filter.to_lowercase();
        held = kept(&held, &move |concept: &Coding| {
            concept.code.to_lowercase().contains(&wanted)
                || concept
                    .display
                    .as_deref()
                    .is_some_and(|text| text.to_lowercase().contains(&wanted))
        });
    }
    held = held.iter().map(|concept| rendered(concept, request)).collect();
    if request.exclude_nested {
        held = flattened(&held);
    }
    let total = counted(&held);
    let paged: Vec<Coding> = held
        .into_iter()
        .skip(request.offset)
        .take(request.count.unwrap_or(usize::MAX))
        .collect();
    Ok(Expansion {
        id: text_of(set, "id"),
        url: text_of(set, "url"),
        version: text_of(set, "version"),
        name: text_of(set, "name"),
        status: text_of(set, "status"),
        total,
        offset: request.offset,
        concepts: paged,
    })
}

fn versioned(set: &Value, request: &ExpansionRequest) -> Result<(), Error> {
    if let Some(wanted) = &request.value_set_version {
        let held = set.get("version").and_then(Value::as_str);
        if held != Some(wanted.as_str()) {
            return Err(Error::NotFound);
        }
    }
    if let Some(wanted) = &request.date {
        if let Some(held) = set.get("date").and_then(Value::as_str) {
            if held.as_bytes() > wanted.as_bytes() {
                return Err(Error::NotFound);
            }
        }
    }
    Ok(())
}

fn pre_expanded(set: &Value) -> Vec<Coding> {
    let system = None;
    set.get("expansion")
        .and_then(|expansion| expansion.get("contains"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let mut held = coding(
                        item,
                        item.get("system").and_then(Value::as_str).or(system),
                        item.get("version").and_then(Value::as_str),
                    )?;
                    held.contains = pre_expanded(&serde_json::json!({"expansion": item}));
                    Some(held)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn composed(
    set: &Value,
    systems: &[Value],
    request: &ExpansionRequest,
) -> Result<Vec<Coding>, Error> {
    let mut held: Vec<Coding> = Vec::new();
    for include in rules(set, "include") {
        held.extend(selected(&include, systems, request)?);
    }
    for exclude in rules(set, "exclude") {
        let dropped = selected(&exclude, systems, request)?;
        let codes: Vec<String> = flattened(&dropped)
            .into_iter()
            .map(|concept| concept.code)
            .collect();
        held = kept(&held, &move |concept: &Coding| {
            !codes.contains(&concept.code)
        });
    }
    Ok(held)
}

fn rules(set: &Value, name: &str) -> Vec<Value> {
    set.get("compose")
        .and_then(|compose| compose.get(name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn selected(
    rule: &Value,
    systems: &[Value],
    request: &ExpansionRequest,
) -> Result<Vec<Coding>, Error> {
    let Some(url) = rule.get("system").and_then(Value::as_str) else {
        if rule.get("valueSet").is_some() {
            return Err(Error::UnsupportedParameter(
                "a compose rule selecting another value set".to_owned(),
            ));
        }
        return Err(Error::InvalidParameter(
            "a compose rule names neither a system nor a value set".to_owned(),
        ));
    };
    let found = systems
        .iter()
        .find(|held| held.get("url").and_then(Value::as_str) == Some(url))
        .ok_or_else(|| {
            Error::InvalidParameter(format!("code system {url:?} is not held by this server"))
        })?;
    if let Some((_, wanted)) = request
        .system_versions
        .iter()
        .find(|(system, _)| system == url)
    {
        let held = found.get("version").and_then(Value::as_str);
        if held != Some(wanted.as_str()) {
            return Err(Error::InvalidParameter(format!(
                "code system {url:?} is not held at version {wanted:?}"
            )));
        }
    }
    let held = concepts(found);
    if let Some(codes) = rule.get("concept").and_then(Value::as_array) {
        let wanted: Vec<&str> = codes
            .iter()
            .filter_map(|item| item.get("code").and_then(Value::as_str))
            .collect();
        return Ok(wanted
            .iter()
            .filter_map(|code| subtree(&held, code).map(flat))
            .collect());
    }
    if let Some(filters) = rule.get("filter").and_then(Value::as_array) {
        let mut out = Vec::new();
        for filter in filters {
            let op = filter.get("op").and_then(Value::as_str);
            let value = filter.get("value").and_then(Value::as_str).unwrap_or_default();
            match op {
                Some("is-a") => {
                    if let Some(found) = subtree(&held, value) {
                        out.push(found.clone());
                    }
                }
                Some("descendent-of") => {
                    if let Some(found) = subtree(&held, value) {
                        out.extend(found.contains.iter().cloned());
                    }
                }
                other => {
                    return Err(Error::UnsupportedParameter(format!(
                        "filter operation {:?}",
                        other.unwrap_or_default()
                    )))
                }
            }
        }
        return Ok(out);
    }
    Ok(held)
}

fn kept(held: &[Coding], wanted: &dyn Fn(&Coding) -> bool) -> Vec<Coding> {
    let mut out = Vec::new();
    for concept in held {
        let contains = kept(&concept.contains, wanted);
        match wanted(concept) {
            true => out.push(Coding {
                contains,
                ..flat(concept)
            }),
            false => out.extend(contains),
        }
    }
    out
}

fn rendered(concept: &Coding, request: &ExpansionRequest) -> Coding {
    let display = match &request.display_language {
        None => concept.display.clone(),
        Some(language) => concept
            .designations
            .iter()
            .find(|held| held.language.as_deref() == Some(language.as_str()))
            .map(|held| held.value.clone())
            .or_else(|| concept.display.clone()),
    };
    Coding {
        display,
        designations: match request.designations {
            true => concept.designations.clone(),
            false => Vec::new(),
        },
        contains: concept
            .contains
            .iter()
            .map(|held| rendered(held, request))
            .collect(),
        ..flat(concept)
    }
}

fn text_of(set: &Value, name: &str) -> Option<String> {
    set.get(name).and_then(Value::as_str).map(str::to_owned)
}

fn counted(held: &[Coding]) -> usize {
    held.iter()
        .map(|concept| 1 + counted(&concept.contains))
        .sum()
}

pub fn expansion_json(
    expansion: &Expansion,
    request: &ExpansionRequest,
    stamp: &Stamp,
) -> Value {
    let mut body = Map::new();
    body.insert(
        "resourceType".to_owned(),
        Value::String("ValueSet".to_owned()),
    );
    for (name, held) in [
        ("id", &expansion.id),
        ("url", &expansion.url),
        ("version", &expansion.version),
        ("name", &expansion.name),
        ("status", &expansion.status),
    ] {
        if let Some(found) = held {
            body.insert(name.to_owned(), Value::String(found.clone()));
        }
    }
    let mut held = Map::new();
    held.insert("identifier".to_owned(), Value::String(stamp.identifier.clone()));
    held.insert("timestamp".to_owned(), Value::String(stamp.timestamp.clone()));
    held.insert("total".to_owned(), Value::from(expansion.total));
    if request.count.is_some() || request.offset > 0 {
        held.insert("offset".to_owned(), Value::from(expansion.offset));
    }
    let listed = asked(request);
    if !listed.is_empty() {
        held.insert("parameter".to_owned(), Value::Array(listed));
    }
    held.insert(
        "contains".to_owned(),
        Value::Array(expansion.concepts.iter().map(contained).collect()),
    );
    body.insert("expansion".to_owned(), Value::Object(held));
    Value::Object(body)
}

fn asked(request: &ExpansionRequest) -> Vec<Value> {
    let mut held = Vec::new();
    if let Some(filter) = &request.filter {
        held.push(serde_json::json!({"name": "filter", "valueString": filter}));
    }
    if let Some(language) = &request.display_language {
        held.push(serde_json::json!({"name": "displayLanguage", "valueCode": language}));
    }
    if request.active_only {
        held.push(serde_json::json!({"name": "activeOnly", "valueBoolean": true}));
    }
    if request.exclude_nested {
        held.push(serde_json::json!({"name": "excludeNested", "valueBoolean": true}));
    }
    if request.designations {
        held.push(serde_json::json!({"name": "includeDesignations", "valueBoolean": true}));
    }
    for (system, version) in &request.system_versions {
        held.push(serde_json::json!({
            "name": "system-version",
            "valueUri": format!("{system}|{version}")
        }));
    }
    held
}

fn contained(concept: &Coding) -> Value {
    let mut held = Map::new();
    if let Some(system) = &concept.system {
        held.insert("system".to_owned(), Value::String(system.clone()));
    }
    if let Some(version) = &concept.version {
        held.insert("version".to_owned(), Value::String(version.clone()));
    }
    held.insert("code".to_owned(), Value::String(concept.code.clone()));
    if let Some(display) = &concept.display {
        held.insert("display".to_owned(), Value::String(display.clone()));
    }
    if concept.inactive {
        held.insert("inactive".to_owned(), Value::Bool(true));
    }
    if !concept.designations.is_empty() {
        held.insert(
            "designation".to_owned(),
            Value::Array(
                concept
                    .designations
                    .iter()
                    .map(|designation| match &designation.language {
                        Some(language) => serde_json::json!({
                            "language": language,
                            "value": designation.value
                        }),
                        None => serde_json::json!({"value": designation.value}),
                    })
                    .collect(),
            ),
        );
    }
    if !concept.contains.is_empty() {
        held.insert(
            "contains".to_owned(),
            Value::Array(concept.contains.iter().map(contained).collect()),
        );
    }
    Value::Object(held)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> Value {
        serde_json::json!({
            "resourceType": "CodeSystem",
            "url": "urn:cs",
            "concept": [{"code": "a", "concept": [{"code": "b"}]}]
        })
    }

    #[test]
    fn a_pre_expanded_set_is_taken_as_it_stands() {
        let set = serde_json::json!({
            "resourceType": "ValueSet",
            "expansion": {"contains": [{"system": "urn:s", "code": "x", "display": "X"}]}
        });
        let expansion = expand(&set, &[], &ExpansionRequest::default()).unwrap();
        assert_eq!(expansion.total, 1);
        assert_eq!(expansion.concepts[0].display.as_deref(), Some("X"));
    }

    #[test]
    fn an_unsupported_filter_operation_is_refused() {
        let set = serde_json::json!({
            "resourceType": "ValueSet",
            "compose": {"include": [{
                "system": "urn:cs",
                "filter": [{"property": "concept", "op": "regex", "value": "a.*"}]
            }]}
        });
        assert!(expand(&set, &[system()], &ExpansionRequest::default()).is_err());
    }

    #[test]
    fn a_descendent_filter_drops_the_code_it_starts_at() {
        let set = serde_json::json!({
            "resourceType": "ValueSet",
            "compose": {"include": [{
                "system": "urn:cs",
                "filter": [{"property": "concept", "op": "descendent-of", "value": "a"}]
            }]}
        });
        let expansion = expand(&set, &[system()], &ExpansionRequest::default()).unwrap();
        assert_eq!(expansion.concepts.len(), 1);
        assert_eq!(expansion.concepts[0].code, "b");
    }

    #[test]
    fn a_rule_naming_neither_a_system_nor_a_value_set_is_refused() {
        let set = serde_json::json!({
            "resourceType": "ValueSet",
            "compose": {"include": [{"concept": [{"code": "a"}]}]}
        });
        let error = expand(&set, &[system()], &ExpansionRequest::default()).unwrap_err();
        assert_eq!(error.http_status(), 400);
        assert!(matches!(error, Error::InvalidParameter(_)));
    }

    #[test]
    fn a_rule_selecting_another_value_set_is_refused_rather_than_dropped() {
        let set = serde_json::json!({
            "resourceType": "ValueSet",
            "compose": {"include": [{"valueSet": ["urn:other"]}]}
        });
        let error = expand(&set, &[system()], &ExpansionRequest::default()).unwrap_err();
        assert_eq!(error.http_status(), 400);
        assert!(matches!(error, Error::UnsupportedParameter(_)));
    }
}
