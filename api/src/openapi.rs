













use crate::app::{Route, Verb};
use fhir_core::FhirVersion;
use serde_json::{json, Map, Value};




const DESCRIBED: &[(&str, &str)] = &[
    ("/", "Search across every type, or process a bundle"),
    (
        "/health",
        "Whether this instance and its dependencies answer",
    ),
    ("/Binary", "Store a binary as itself rather than as JSON"),
    ("/Binary/{id}", "Read or replace a stored binary"),
    ("/$liveness", "Whether the process is running"),
    (
        "/$readiness",
        "Whether the instance is ready to be sent work",
    ),
    ("/metadata", "The capability statement"),
    ("/openapi.json", "This description"),
    (
        "/$reset",
        "Empty this instance, where an operator allowed it",
    ),
    (
        "/$fhirUser-lookup",
        "Find the one user a set of search parameters names",
    ),
    ("/$versions", "The releases this instance serves"),
    (
        "/.well-known/smart-configuration",
        "The SMART discovery document",
    ),
    ("/_introspect", "Introspect a token (RFC 7662)"),
    (
        "/SearchParameter/$status",
        "The search parameters installed and what state each is in",
    ),
    (
        "/AuditEvent/$verify",
        "Verify the seal over the audit trail",
    ),
    ("/AuditEvent/$export-trail", "Export the audit trail"),
    (
        "/AuditEvent/$retain",
        "Apply the retention rule to the audit trail",
    ),
    (
        "/SearchParameter/$status/_search",
        "The parameter states, asked for by POST",
    ),
    (
        "/SearchParameter/{id}/$status",
        "The state of one search parameter",
    ),
    (
        "/SearchParameter/$reindex",
        "Rebuild the indexes for the installed parameters",
    ),
    (
        "/SearchParameter/$refresh",
        "Re-read the installed search parameters",
    ),
    (
        "/CompartmentDefinition",
        "The compartments this build knows",
    ),
    ("/OperationDefinition", "The operations this build answers"),
    ("/OperationDefinition/{code}", "One operation's definition"),
    (
        "/CompartmentDefinition/{id}",
        "One compartment's definition",
    ),
    (
        "/{type}/{id}/{target}",
        "Search a compartment of one resource",
    ),
    (
        "/{type}/{id}",
        "Read, replace, delete or patch one resource",
    ),
    ("/_search", "Search across every type, asked for by POST"),
    ("/{type}/_search", "Search one type, asked for by POST"),
    ("/_history", "The history of every type"),
    ("/{type}/_history", "The history of one type"),
    ("/{type}/{id}/_history", "The history of one resource"),
    ("/{type}/{id}/_history/{vid}", "One version of one resource"),
    (
        "/{type}",
        "Search a type, create in it, or act on what a query selects",
    ),
    (
        "/{type}/{id}/$purge-history",
        "Remove the versions behind the current one",
    ),
    ("/$convert", "Convert a resource between releases"),
    (
        "/StructureDefinition/$snapshot",
        "Generate a profile's snapshot from its differential",
    ),
    (
        "/ValueSet/$validate-code",
        "Whether a code is in a value set",
    ),
    (
        "/ValueSet/{id}/$validate-code",
        "Whether a code is in this value set",
    ),
    (
        "/CodeSystem/$validate-code",
        "Whether a code is in a code system",
    ),
    (
        "/CodeSystem/{id}/$validate-code",
        "Whether a code is in this code system",
    ),
    ("/CodeSystem/$lookup", "What a code means"),
    ("/CodeSystem/$subsumes", "Whether one code subsumes another"),
    (
        "/CodeSystem/$find-matches",
        "The codes matching a set of properties",
    ),
    ("/CodeSystem/$compose", "Compose a code from properties"),
    ("/ConceptMap/$translate", "Translate a code through a map"),
    (
        "/ConceptMap/{id}/$translate",
        "Translate a code through this map",
    ),
    ("/$closure", "Maintain a transitive closure table"),
    (
        "/Observation/$lastn",
        "The most recent observations per code",
    ),
    ("/{type}/{id}/$erase", "Erase a resource and its history"),
    (
        "/{type}/{id}/_history/{vid}/$erase",
        "Erase one version of a resource",
    ),
    (
        "/Patient/{id}/$purge",
        "Erase a patient and the records in their compartment",
    ),
    (
        "/Composition/$document",
        "Build a document from a composition",
    ),
    (
        "/Composition/{id}/$document",
        "Build a document from this composition",
    ),
    ("/$meta", "The meta values in use across every type"),
    ("/{type}/$meta", "The meta values in use in one type"),
    ("/{type}/{id}/$meta", "The meta of one resource"),
    ("/{type}/{id}/$meta-add", "Add to the meta of one resource"),
    (
        "/{type}/{id}/$meta-delete",
        "Remove from the meta of one resource",
    ),
    (
        "/Patient/{id}/$everything",
        "Everything in a patient's compartment",
    ),
    ("/Patient/$member-match", "Match a member across two payers"),
    ("/$includes", "The resources a set of references reaches"),
    (
        "/{type}/$includes",
        "The resources one type's references reach",
    ),
    (
        "/DocumentReference/$docref",
        "The document references for a patient",
    ),
    ("/ValueSet/$expand", "Expand a value set"),
    ("/$convert-data", "Convert other formats into FHIR"),
    ("/{type}/$validate", "Validate a resource of this type"),
    (
        "/{type}/{id}/$validate",
        "Validate against a stored resource",
    ),
    ("/$export", "Export everything, as a job"),
    (
        "/Patient/$export",
        "Export the patient compartment, as a job",
    ),
    ("/Group/{id}/$export", "Export a group's members, as a job"),
    ("/$import", "Import ndjson, as a job"),
    ("/$bulk-delete", "Delete what a query selects, as a job"),
    (
        "/{type}/$bulk-delete",
        "Delete what a query selects in one type, as a job",
    ),
    (
        "/$bulk-delete-soft-deleted",
        "Remove what was deleted, for good, as a job",
    ),
    (
        "/{type}/$bulk-delete-soft-deleted",
        "Remove one type's deleted resources, for good, as a job",
    ),
    ("/$bulk-update", "Patch what a query selects, as a job"),
    (
        "/{type}/$bulk-update",
        "Patch what a query selects in one type, as a job",
    ),
    ("/$reindex", "Rebuild every index, as a job"),
    (
        "/{type}/{id}/$reindex",
        "Rebuild the indexes of one resource",
    ),
    ("/_jobs/{id}", "Poll or cancel a job"),
    ("/_jobs/{id}/{*name}", "Read a file a job produced"),
];

pub const FHIR_JSON: &str = "application/fhir+json";


pub fn description_of(path: &str) -> Option<&'static str> {
    DESCRIBED
        .iter()
        .find(|(held, _)| *held == path)
        .map(|(_, description)| *description)
}



pub fn described() -> Vec<&'static str> {
    DESCRIBED.iter().map(|(path, _)| *path).collect()
}


pub fn document(version: FhirVersion, routes: &[Route]) -> Value {
    let mut paths = Map::new();
    for route in routes {
        let mut operations = Map::new();
        for verb in route.methods {
            operations.insert(
                verb.as_str().to_ascii_lowercase(),
                operation(route, *verb, version),
            );
        }
        let held = named(route.path);
        if !held.is_empty() {
            operations.insert("parameters".to_owned(), Value::Array(held));
        }
        paths.insert(openapi_path(route.path), Value::Object(operations));
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": format!("FHIR {} API", version.as_str()),
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Generated from the route table this instance serves. \
                            The capability statement at /metadata is generated from \
                            the same table.",
        },
        "paths": Value::Object(paths),
    })
}

fn operation(route: &Route, verb: Verb, version: FhirVersion) -> Value {
    let summary = description_of(route.path).unwrap_or("");
    let mut held = json!({
        "summary": summary,
        "operationId": operation_id(route.path, verb),
        "responses": responses(verb),
        "tags": [tag_of(route.path)],
    });
    if matches!(verb, Verb::Post | Verb::Put | Verb::Patch) {
        held["requestBody"] = json!({
            "required": true,
            "content": {FHIR_JSON: {"schema": {"type": "object"}}},
        });
    }
    if matches!(verb, Verb::Get) && route.path.contains("{type}") {
        held["description"] = json!(format!(
            "The types are those FHIR {} publishes.",
            version.as_str()
        ));
    }
    held
}

fn responses(verb: Verb) -> Value {
    let answer = |description: &str| {
        json!({
            "description": description,
            "content": {FHIR_JSON: {"schema": {"type": "object"}}},
        })
    };
    let mut held = Map::new();
    match verb {
        Verb::Post => {
            held.insert("200".to_owned(), answer("The answer to the request"));
            held.insert("201".to_owned(), answer("What was created"));
        }
        Verb::Put => {
            held.insert("200".to_owned(), answer("What was replaced"));
            held.insert("201".to_owned(), answer("What was created"));
        }
        Verb::Delete => {
            held.insert(
                "204".to_owned(),
                json!({"description": "Deleted, or already absent"}),
            );
        }
        _ => {
            held.insert("200".to_owned(), answer("The answer to the request"));
        }
    }
    for (status, description) in [
        ("400", "The request was not understood"),
        ("401", "No credential, or one that does not verify"),
        ("403", "Out of scope"),
        ("404", "Not found"),
        ("500", "The instance failed"),
    ] {
        held.insert(status.to_owned(), answer(description));
    }
    Value::Object(held)
}



fn named(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|part| {
            let held = part.strip_prefix('{')?.strip_suffix('}')?;
            Some(held.trim_start_matches('*').to_owned())
        })
        .map(|name| {
            json!({
                "name": name,
                "in": "path",
                "required": true,
                "schema": {"type": "string"},
            })
        })
        .collect()
}

fn openapi_path(path: &str) -> String {
    path.replace("{*", "{")
}

fn operation_id(path: &str, verb: Verb) -> String {
    let held: String = path
        .chars()
        .map(|held| match held.is_ascii_alphanumeric() {
            true => held,
            false => '_',
        })
        .collect();
    format!(
        "{}{}",
        verb.as_str().to_ascii_lowercase(),
        held.trim_end_matches('_')
    )
}



fn tag_of(path: &str) -> &str {
    path.split('/')
        .find(|part| !part.is_empty() && !part.starts_with('{'))
        .unwrap_or("system")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wildcard_becomes_a_plain_parameter() {
        assert_eq!(openapi_path("/_jobs/{id}/{*name}"), "/_jobs/{id}/{name}");
        let held = named("/_jobs/{id}/{*name}");
        assert_eq!(held.len(), 2);
        assert_eq!(held[1]["name"], "name");
    }

    #[test]
    fn an_operation_id_is_one_word() {
        assert_eq!(operation_id("/{type}/{id}", Verb::Get), "get__type___id");
        assert!(!operation_id("/$export", Verb::Post).contains('$'));
    }

    #[test]
    fn a_path_is_tagged_by_the_first_thing_that_is_not_a_placeholder() {
        assert_eq!(tag_of("/Patient/{id}/$everything"), "Patient");
        assert_eq!(tag_of("/{type}/{id}"), "system");
    }
}
