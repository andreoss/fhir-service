use fhir_core::terminology::{
    ancestors, concepts, descendants, expand, expansion_json, ExpansionRequest, Stamp,
};
use serde_json::json;

fn system() -> serde_json::Value {
    json!({
        "resourceType": "CodeSystem",
        "url": "urn:cs",
        "version": "1.0",
        "concept": [{
            "code": "top",
            "display": "Top",
            "designation": [{"language": "nl", "value": "Boven"}],
            "concept": [
                {"code": "mid", "display": "Middle", "concept": [{"code": "leaf", "display": "Leaf"}]},
                {"code": "old", "display": "Old", "property": [{"code": "status", "valueCode": "retired"}]}
            ]
        }]
    })
}

fn set() -> serde_json::Value {
    json!({
        "resourceType": "ValueSet",
        "id": "vs-1",
        "url": "urn:vs",
        "version": "2.0",
        "date": "2026-01-01",
        "compose": {"include": [{"system": "urn:cs"}]}
    })
}

fn asked() -> ExpansionRequest {
    ExpansionRequest::default()
}

#[test]
fn a_code_system_yields_its_hierarchy() {
    let held = concepts(&system());
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].code, "top");
    assert_eq!(held[0].contains.len(), 2);
    assert_eq!(held[0].contains[0].contains[0].code, "leaf");
    assert!(held[0].contains[1].inactive);
    assert_eq!(held[0].designations[0].value, "Boven");
}

#[test]
fn subsumption_walks_the_hierarchy_in_both_directions() {
    let under: Vec<String> = descendants(&system(), "mid")
        .iter()
        .map(|held| held.code.clone())
        .collect();
    assert_eq!(under, vec!["mid".to_owned(), "leaf".to_owned()]);
    let over: Vec<String> = ancestors(&system(), "leaf")
        .iter()
        .map(|held| held.code.clone())
        .collect();
    assert_eq!(
        over,
        vec!["leaf".to_owned(), "mid".to_owned(), "top".to_owned()]
    );
    assert!(descendants(&system(), "nonesuch").is_empty());
    assert!(ancestors(&system(), "nonesuch").is_empty());
}

#[test]
fn a_value_set_expands_over_the_systems_it_composes() {
    let systems = vec![system()];
    let expansion = expand(&set(), &systems, &asked()).unwrap();
    assert_eq!(expansion.total, 4);
    assert_eq!(expansion.concepts.len(), 1);
    assert_eq!(expansion.concepts[0].code, "top");
}

#[test]
fn nesting_is_dropped_when_it_is_excluded() {
    let systems = vec![system()];
    let request = ExpansionRequest {
        exclude_nested: true,
        ..asked()
    };
    let expansion = expand(&set(), &systems, &request).unwrap();
    assert_eq!(expansion.concepts.len(), 4);
    assert!(expansion
        .concepts
        .iter()
        .all(|held| held.contains.is_empty()));
}

#[test]
fn a_filter_and_paging_narrow_the_expansion() {
    let systems = vec![system()];
    let filtered = expand(
        &set(),
        &systems,
        &ExpansionRequest {
            filter: Some("lea".to_owned()),
            exclude_nested: true,
            ..asked()
        },
    )
    .unwrap();
    assert_eq!(
        filtered
            .concepts
            .iter()
            .map(|held| held.code.clone())
            .collect::<Vec<String>>(),
        vec!["leaf".to_owned()]
    );
    let paged = expand(
        &set(),
        &systems,
        &ExpansionRequest {
            exclude_nested: true,
            count: Some(2),
            offset: 1,
            ..asked()
        },
    )
    .unwrap();
    assert_eq!(paged.total, 4);
    assert_eq!(paged.offset, 1);
    assert_eq!(paged.concepts.len(), 2);
}

#[test]
fn inactive_concepts_are_dropped_when_only_active_are_asked_for() {
    let systems = vec![system()];
    let request = ExpansionRequest {
        exclude_nested: true,
        active_only: true,
        ..asked()
    };
    let expansion = expand(&set(), &systems, &request).unwrap();
    assert!(expansion.concepts.iter().all(|held| held.code != "old"));
    assert_eq!(expansion.total, 3);
}

#[test]
fn a_display_language_and_designations_are_honoured() {
    let systems = vec![system()];
    let request = ExpansionRequest {
        exclude_nested: true,
        display_language: Some("nl".to_owned()),
        designations: true,
        ..asked()
    };
    let expansion = expand(&set(), &systems, &request).unwrap();
    let top = expansion
        .concepts
        .iter()
        .find(|held| held.code == "top")
        .unwrap();
    assert_eq!(top.display.as_deref(), Some("Boven"));
    let rendered = expansion_json(&expansion, &request, &stamp());
    assert_eq!(
        rendered["expansion"]["contains"][0]["designation"][0]["value"],
        "Boven"
    );
    assert_eq!(rendered["resourceType"], "ValueSet");
    assert_eq!(rendered["expansion"]["total"], 4);
}

#[test]
fn a_set_no_version_of_which_answers_the_request_is_not_found() {
    let systems = vec![system()];
    let older = ExpansionRequest {
        date: Some("2025-01-01".to_owned()),
        ..asked()
    };
    let error = expand(&set(), &systems, &older).unwrap_err();
    assert_eq!(error.http_status(), 404, "{error:?}");
    let version = ExpansionRequest {
        value_set_version: Some("9.9".to_owned()),
        ..asked()
    };
    let error = expand(&set(), &systems, &version).unwrap_err();
    assert_eq!(error.http_status(), 404, "{error:?}");
}

#[test]
fn a_request_naming_a_version_the_server_cannot_honour_is_a_bad_request() {
    let systems = vec![system()];
    let pinned = ExpansionRequest {
        system_versions: vec![("urn:cs".to_owned(), "9.9".to_owned())],
        ..asked()
    };
    let error = expand(&set(), &systems, &pinned).unwrap_err();
    assert_eq!(error.http_status(), 400, "{error:?}");
    let unknown = expand(&set(), &[], &asked()).unwrap_err();
    assert_eq!(unknown.http_status(), 400, "{unknown:?}");
}

#[test]
fn the_version_and_the_date_the_set_carries_are_honoured() {
    let systems = vec![system()];
    let held = ExpansionRequest {
        value_set_version: Some("2.0".to_owned()),
        date: Some("2026-06-01".to_owned()),
        ..asked()
    };
    assert!(expand(&set(), &systems, &held).is_ok());
    let pinned = ExpansionRequest {
        system_versions: vec![("urn:cs".to_owned(), "1.0".to_owned())],
        ..asked()
    };
    let expansion = expand(&set(), &systems, &pinned).expect("the pinned version is the one held");
    assert_eq!(expansion.total, 4);
}

#[test]
fn a_composed_subtree_and_an_exclusion_are_honoured() {
    let composed = json!({
        "resourceType": "ValueSet",
        "url": "urn:vs2",
        "compose": {
            "include": [{"system": "urn:cs", "filter": [{"property": "concept", "op": "is-a", "value": "mid"}]}],
            "exclude": [{"system": "urn:cs", "concept": [{"code": "leaf"}]}]
        }
    });
    let systems = vec![system()];
    let expansion = expand(
        &composed,
        &systems,
        &ExpansionRequest {
            exclude_nested: true,
            ..asked()
        },
    )
    .unwrap();
    assert_eq!(
        expansion
            .concepts
            .iter()
            .map(|held| held.code.clone())
            .collect::<Vec<String>>(),
        vec!["mid".to_owned()]
    );
}

#[test]
fn an_expansion_is_stamped_and_carries_no_empty_element() {
    let expansion = expand(&set(), &[system()], &asked()).unwrap();
    let rendered = expansion_json(&expansion, &asked(), &stamp());
    assert_eq!(
        rendered["expansion"]["timestamp"],
        "2026-09-07T00:00:00.000Z"
    );
    assert_eq!(
        rendered["expansion"]["identifier"],
        "urn:uuid:00000000-0000-4000-8000-000000000000"
    );
    assert!(rendered["expansion"]["parameter"].is_null());
    let mut request = asked();
    request.active_only = true;
    let held = expand(&set(), &[system()], &request).unwrap();
    let rendered = expansion_json(&held, &request, &stamp());
    assert!(rendered["expansion"]["parameter"].is_array());
}

fn stamp() -> Stamp {
    Stamp {
        identifier: "urn:uuid:00000000-0000-4000-8000-000000000000".to_owned(),
        timestamp: "2026-09-07T00:00:00.000Z".to_owned(),
    }
}

#[test]
fn an_expansion_carries_an_offset_only_when_it_is_paged() {
    let plain = expand(&set(), &[system()], &asked()).unwrap();
    let rendered = expansion_json(&plain, &asked(), &stamp());
    assert!(
        rendered["expansion"].get("offset").is_none(),
        "{}",
        rendered["expansion"]
    );

    for request in [
        ExpansionRequest {
            count: Some(2),
            ..asked()
        },
        ExpansionRequest {
            offset: 1,
            ..asked()
        },
    ] {
        let paged = expand(&set(), &[system()], &request).unwrap();
        let rendered = expansion_json(&paged, &request, &stamp());
        assert_eq!(rendered["expansion"]["offset"], request.offset);
    }
}

#[test]
fn an_expansion_reports_the_whole_count_beside_the_page_it_carries() {
    let request = ExpansionRequest {
        exclude_nested: true,
        count: Some(2),
        offset: 1,
        ..asked()
    };
    let paged = expand(&set(), &[system()], &request).unwrap();
    assert_eq!(paged.total, 4);
    assert_eq!(paged.concepts.len(), 2);
    let rendered = expansion_json(&paged, &request, &stamp());
    assert_eq!(rendered["expansion"]["total"], 4);
    assert_eq!(rendered["expansion"]["offset"], 1);
    assert_eq!(
        rendered["expansion"]["contains"].as_array().map(Vec::len),
        Some(2)
    );
}
