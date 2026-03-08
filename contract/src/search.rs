use fhir_core::search::{
    lookup, Chain, ChainDirection, Compartment, Criterion, Filter, Grant, GrantFilter, Include,
    IncludeDirection, Modifier, SearchValue,
};
use fhir_core::{ResourceEnvelope, ResourceType};
use fhir_store::{SearchQuery, SortDirection, SortKey, TotalMode};

use crate::fixture::{envelope, id, observation, patient};

fn kind(name: &str) -> ResourceType {
    name.parse().expect("a known resource type")
}

pub fn filter(resource_type: &str, name: &str, raw: &str) -> Filter {
    let def = lookup(Some(kind(resource_type)), name).expect("a built-in parameter");
    let values = raw
        .split(',')
        .map(|part| def.value(part).expect("a valid value"))
        .collect();
    Filter::new(name, def.target.clone(), values)
}

pub fn qualified(resource_type: &str, name: &str, modifier: Modifier, raw: &str) -> Filter {
    let def = lookup(Some(kind(resource_type)), name).expect("a built-in parameter");
    let values = raw
        .split(',')
        .map(|part| def.value_with(&modifier, part).expect("a valid value"))
        .collect();
    Filter {
        modifier,
        ..Filter::new(name, def.target.clone(), values)
    }
}

fn query(resource_type: &str, filters: Vec<Filter>) -> SearchQuery {
    SearchQuery {
        filters,
        ..SearchQuery::of_type(kind(resource_type))
    }
}

fn ids(page: &fhir_store::SearchPage) -> Vec<String> {
    page.entries
        .iter()
        .map(|entry| entry.id().as_str().to_owned())
        .collect()
}

fn included(page: &fhir_store::SearchPage) -> Vec<String> {
    let mut found: Vec<String> = page
        .included
        .iter()
        .map(|entry| entry.id().as_str().to_owned())
        .collect();
    found.sort();
    found
}

fn list(id: &str, entries: &[&str]) -> ResourceEnvelope {
    let items: Vec<String> = entries
        .iter()
        .map(|reference| format!(r#"{{"item":{{"reference":"{reference}"}}}}"#))
        .collect();
    envelope(
        "List",
        id,
        &format!(
            r#""status":"current","mode":"working","entry":[{}]"#,
            items.join(",")
        ),
    )
}

fn code_set(id: &str, url: &str, codes: &[&str]) -> ResourceEnvelope {
    let concepts: Vec<String> = codes
        .iter()
        .map(|code| format!(r#"{{"code":"{code}"}}"#))
        .collect();
    envelope(
        "ValueSet",
        id,
        &format!(
            r#""url":"{url}","status":"active","compose":{{"include":[{{"system":"urn:s","concept":[{}]}}]}}"#,
            concepts.join(",")
        ),
    )
}

async fn seeded(store: &dyn fhir_store::ResourceStore) {
    store.create(patient("p1", "Stone", true)).await.unwrap();
    store.create(patient("p2", "Rivers", false)).await.unwrap();
    store
        .create(patient("p3", "Stonewall", true))
        .await
        .unwrap();
    store
        .create(observation("v1", "code-1", 4.5, "Patient/p1"))
        .await
        .unwrap();
    store
        .create(observation("v2", "code-2", 9.0, "Patient/p2"))
        .await
        .unwrap();
}

pub async fn selection(store: &dyn fhir_store::ResourceStore) {
    seeded(store).await;

    let by_id = store
        .search(&query("Patient", vec![filter("Patient", "_id", "p1")]))
        .await
        .unwrap();
    assert_eq!(ids(&by_id), vec!["p1"]);
    assert_eq!(by_id.total, Some(1));

    let by_flag = store
        .search(&query("Patient", vec![filter("Patient", "active", "true")]))
        .await
        .unwrap();
    let mut found = ids(&by_flag);
    found.sort();
    assert_eq!(found, vec!["p1", "p3"]);

    let by_name = store
        .search(&query("Patient", vec![filter("Patient", "name", "sto")]))
        .await
        .unwrap();
    let mut found = ids(&by_name);
    found.sort();
    assert_eq!(found, vec!["p1", "p3"]);

    let by_code = store
        .search(&query(
            "Observation",
            vec![filter("Observation", "code", "urn:s|code-1")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&by_code), vec!["v1"]);

    let by_wrong_system = store
        .search(&query(
            "Observation",
            vec![filter("Observation", "code", "urn:other|code-1")],
        ))
        .await
        .unwrap();
    assert!(by_wrong_system.entries.is_empty());

    let by_quantity = store
        .search(&query(
            "Observation",
            vec![filter("Observation", "value-quantity", "gt5|urn:u|mg")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&by_quantity), vec!["v2"]);

    let by_reference = store
        .search(&query(
            "Observation",
            vec![filter("Observation", "subject", "Patient/p1")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&by_reference), vec!["v1"]);

    let by_bare_reference = store
        .search(&query(
            "Observation",
            vec![filter("Observation", "subject", "p2")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&by_bare_reference), vec!["v2"]);

    let by_date = store
        .search(&query(
            "Patient",
            vec![filter("Patient", "birthdate", "1980")],
        ))
        .await
        .unwrap();
    assert_eq!(by_date.total, Some(3));

    let alternatives = store
        .search(&query("Patient", vec![filter("Patient", "_id", "p1,p2")]))
        .await
        .unwrap();
    assert_eq!(alternatives.total, Some(2));

    let conjunction = store
        .search(&query(
            "Patient",
            vec![
                filter("Patient", "_id", "p1"),
                filter("Patient", "active", "false"),
            ],
        ))
        .await
        .unwrap();
    assert!(conjunction.entries.is_empty());

    let every = store.search(&SearchQuery::default()).await.unwrap();
    assert_eq!(every.total, Some(5));
}

pub async fn qualifiers(store: &dyn fhir_store::ResourceStore) {
    seeded(store).await;
    store
        .create(envelope(
            "Patient",
            "p4",
            r#""active":true,"identifier":[{"system":"urn:i","value":"abc","type":{"coding":[{"system":"urn:t","code":"mr"}]}}]"#,
        ))
        .await
        .unwrap();

    let missing = store
        .search(&query(
            "Patient",
            vec![qualified("Patient", "name", Modifier::Missing, "true")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&missing), vec!["p4"]);

    let present = store
        .search(&query(
            "Patient",
            vec![qualified("Patient", "name", Modifier::Missing, "false")],
        ))
        .await
        .unwrap();
    assert_eq!(present.total, Some(3));

    let exact = store
        .search(&query(
            "Patient",
            vec![qualified("Patient", "name", Modifier::Exact, "Stone")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&exact), vec!["p1"]);

    let contains = store
        .search(&query(
            "Patient",
            vec![qualified("Patient", "name", Modifier::Contains, "onew")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&contains), vec!["p3"]);

    let negated = store
        .search(&query(
            "Observation",
            vec![qualified("Observation", "code", Modifier::Not, "code-1")],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&negated), vec!["v2"]);

    let narrative = store
        .search(&query(
            "Patient",
            vec![qualified(
                "Patient",
                "identifier",
                Modifier::OfType,
                "urn:t|mr|abc",
            )],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&narrative), vec!["p4"]);

    let typed = store
        .search(&query(
            "Observation",
            vec![qualified(
                "Observation",
                "subject",
                Modifier::Type(kind("Patient")),
                "p1",
            )],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&typed), vec!["v1"]);

    store
        .create(code_set("s1", "urn:set:one", &["code-1"]))
        .await
        .unwrap();
    let in_set = store
        .search(&query(
            "Observation",
            vec![qualified(
                "Observation",
                "code",
                Modifier::In,
                "urn:set:one",
            )],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&in_set), vec!["v1"]);
    let out_of_set = store
        .search(&query(
            "Observation",
            vec![qualified(
                "Observation",
                "code",
                Modifier::NotIn,
                "urn:set:one",
            )],
        ))
        .await
        .unwrap();
    assert_eq!(ids(&out_of_set), vec!["v2"]);
}

pub async fn ordering(store: &dyn fhir_store::ResourceStore) {
    seeded(store).await;
    let sorted = |direction| SearchQuery {
        sort: vec![SortKey {
            name: "name".to_owned(),
            target: lookup(Some(kind("Patient")), "name")
                .unwrap()
                .target
                .clone(),
            direction,
        }],
        ..SearchQuery::of_type(kind("Patient"))
    };

    let ascending = store
        .search(&sorted(SortDirection::Ascending))
        .await
        .unwrap();
    assert_eq!(ids(&ascending), vec!["p2", "p1", "p3"]);
    let descending = store
        .search(&sorted(SortDirection::Descending))
        .await
        .unwrap();
    assert_eq!(ids(&descending), vec!["p3", "p1", "p2"]);

    let paged = SearchQuery {
        offset: 1,
        count: 1,
        ..sorted(SortDirection::Ascending)
    };
    let page = store.search(&paged).await.unwrap();
    assert_eq!(ids(&page), vec!["p1"]);
    assert_eq!(page.total, Some(3));
    assert_eq!(page.offset, 1);

    let untotalled = SearchQuery {
        total: TotalMode::None,
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert_eq!(store.search(&untotalled).await.unwrap().total, None);

    let estimated = SearchQuery {
        total: TotalMode::Estimate,
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert!(store.search(&estimated).await.unwrap().total.is_some());

    let deleted = store
        .delete(&crate::fixture::key("Patient", "p1"))
        .await
        .unwrap();
    assert!(deleted.is_deleted());
    let after = store
        .search(&SearchQuery::of_type(kind("Patient")))
        .await
        .unwrap();
    assert_eq!(after.total, Some(2));
}

pub async fn linking(store: &dyn fhir_store::ResourceStore) {
    seeded(store).await;
    store.create(list("l1", &["Patient/p1"])).await.unwrap();

    let chained = SearchQuery {
        chains: vec![Chain {
            name: "subject.name".to_owned(),
            link: "subject".to_owned(),
            target: lookup(Some(kind("Observation")), "subject")
                .unwrap()
                .target
                .clone(),
            types: vec![kind("Patient")],
            direction: ChainDirection::Forward,
            next: Box::new(Criterion::Direct(filter("Patient", "name", "Stone"))),
        }],
        ..SearchQuery::of_type(kind("Observation"))
    };
    assert_eq!(ids(&store.search(&chained).await.unwrap()), vec!["v1"]);

    let reverse = SearchQuery {
        chains: vec![Chain {
            name: "_has:Observation:subject:code".to_owned(),
            link: "subject".to_owned(),
            target: lookup(Some(kind("Observation")), "subject")
                .unwrap()
                .target
                .clone(),
            types: vec![kind("Observation")],
            direction: ChainDirection::Reverse,
            next: Box::new(Criterion::Direct(filter("Observation", "code", "code-2"))),
        }],
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert_eq!(ids(&store.search(&reverse).await.unwrap()), vec!["p2"]);

    let with_include = SearchQuery {
        filters: vec![filter("Observation", "code", "code-1")],
        includes: vec![Include {
            name: "subject".to_owned(),
            source: Some(kind("Observation")),
            paths: vec!["subject".to_owned()],
            target: None,
            direction: IncludeDirection::Forward,
            iterate: false,
        }],
        ..SearchQuery::of_type(kind("Observation"))
    };
    let page = store.search(&with_include).await.unwrap();
    assert_eq!(ids(&page), vec!["v1"]);
    assert_eq!(included(&page), vec!["p1"]);
    assert_eq!(page.total, Some(1));

    let with_revinclude = SearchQuery {
        filters: vec![filter("Patient", "_id", "p1")],
        includes: vec![Include {
            name: "subject".to_owned(),
            source: Some(kind("Observation")),
            paths: vec!["subject".to_owned()],
            target: Some(kind("Patient")),
            direction: IncludeDirection::Reverse,
            iterate: false,
        }],
        ..SearchQuery::of_type(kind("Patient"))
    };
    let page = store.search(&with_revinclude).await.unwrap();
    assert_eq!(included(&page), vec!["v1"]);

    let by_list = SearchQuery {
        list: Some(id("l1")),
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert_eq!(ids(&store.search(&by_list).await.unwrap()), vec!["p1"]);

    let in_compartment = SearchQuery {
        compartment: Some(Compartment {
            kind: kind("Patient"),
            id: id("p1"),
        }),
        ..SearchQuery::of_type(kind("Observation"))
    };
    assert_eq!(
        ids(&store.search(&in_compartment).await.unwrap()),
        vec!["v1"]
    );

    let granted = SearchQuery {
        grant: Some(Grant {
            types: vec![kind("Observation")],
            compartments: vec![Compartment {
                kind: kind("Patient"),
                id: id("p2"),
            }],
            filters: Vec::new(),
            every: Vec::new(),
        }),
        ..SearchQuery::default()
    };
    assert_eq!(ids(&store.search(&granted).await.unwrap()), vec!["v2"]);

    let refused = SearchQuery {
        grant: Some(Grant {
            types: vec![kind("Observation")],
            compartments: Vec::new(),
            filters: Vec::new(),
            every: Vec::new(),
        }),
        ..SearchQuery::of_type(kind("Patient"))
    };
    assert!(store.search(&refused).await.unwrap().entries.is_empty());

    let narrowing = Grant {
        types: Vec::new(),
        compartments: Vec::new(),
        filters: vec![GrantFilter {
            resource_type: kind("Observation"),
            filter: filter("Observation", "code", "code-1"),
        }],
        every: Vec::new(),
    };
    let narrowed = SearchQuery {
        grant: Some(narrowing.clone()),
        ..SearchQuery::of_type(kind("Observation"))
    };
    assert_eq!(ids(&store.search(&narrowed).await.unwrap()), vec!["v1"]);

    let untouched = SearchQuery {
        grant: Some(narrowing.clone()),
        ..SearchQuery::of_type(kind("Patient"))
    };
    let mut reached = ids(&store.search(&untouched).await.unwrap());
    reached.sort();
    assert!(
        reached.contains(&"p1".to_owned()) && reached.contains(&"p2".to_owned()),
        "{reached:?}"
    );

    let pulled = SearchQuery {
        filters: vec![filter("Patient", "_id", "p1")],
        includes: vec![Include {
            name: "subject".to_owned(),
            source: Some(kind("Observation")),
            paths: vec!["subject".to_owned()],
            target: Some(kind("Patient")),
            direction: IncludeDirection::Reverse,
            iterate: false,
        }],
        grant: Some(Grant {
            filters: vec![GrantFilter {
                resource_type: kind("Observation"),
                filter: filter("Observation", "code", "code-2"),
            }],
            ..narrowing
        }),
        ..SearchQuery::of_type(kind("Patient"))
    };
    let page = store.search(&pulled).await.unwrap();
    assert_eq!(ids(&page), vec!["p1"]);
    assert!(included(&page).is_empty(), "{:?}", included(&page));
}

pub async fn composites(store: &dyn fhir_store::ResourceStore) {
    store
        .create(observation("c1", "code-1", 4.5, "Patient/p1"))
        .await
        .unwrap();
    store
        .create(observation("c2", "code-2", 4.5, "Patient/p1"))
        .await
        .unwrap();
    let paired = query(
        "Observation",
        vec![filter(
            "Observation",
            "code-value-quantity",
            "urn:s|code-1$4.5|urn:u|mg",
        )],
    );
    assert_eq!(ids(&store.search(&paired).await.unwrap()), vec!["c1"]);
    let mismatched = query(
        "Observation",
        vec![filter(
            "Observation",
            "code-value-quantity",
            "urn:s|code-1$9|urn:u|mg",
        )],
    );
    assert!(store.search(&mismatched).await.unwrap().entries.is_empty());
    let _ = SearchValue::Missing(true);
}

fn band(id: &str, code: &str) -> fhir_core::ResourceEnvelope {
    crate::fixture::envelope(
        "Patient",
        id,
        &format!(r#""extension":[{{"url":"urn:e:band","valueCode":"{code}"}}]"#),
    )
}

fn band_spec() -> fhir_core::search::ParameterSpec {
    fhir_core::search::ParameterSpec::parse(&serde_json::json!({
        "resourceType": "SearchParameter",
        "url": "urn:p:band",
        "status": "active",
        "code": "band",
        "base": ["Patient"],
        "type": "token",
        "expression": "Patient.extension.valueCode"
    }))
    .expect("the suite definition is valid")
}

pub async fn targeted_index(store: &dyn fhir_store::ResourceStore) {
    let spec = band_spec();
    store.create(band("t1", "high")).await.unwrap();
    store.create(band("t2", "low")).await.unwrap();
    store.index_parameter(&spec).await.unwrap();
    store.reindex(std::slice::from_ref(&spec)).await.unwrap();

    let banded = |code: &str| fhir_store::SearchQuery {
        filters: vec![fhir_core::search::Filter {
            index: Some("urn:p:band".to_owned()),
            ..fhir_core::search::Filter::new(
                "band",
                fhir_core::search::Target::path(["extension.valueCode"]),
                vec![fhir_core::search::SearchValue::parse(
                    fhir_core::search::ValueType::Token,
                    code,
                )
                .unwrap()],
            )
        }],
        ..fhir_store::SearchQuery::of_type("Patient".parse().expect("a known type"))
    };
    let found = |page: fhir_store::SearchPage| -> Vec<String> {
        page.entries
            .iter()
            .map(|entry| entry.id().as_str().to_owned())
            .collect()
    };
    assert_eq!(
        found(store.search(&banded("high")).await.unwrap()),
        vec!["t1"]
    );

    store.update(band("t1", "low"), None).await.unwrap();
    let reports = store
        .reindex_resource(
            std::slice::from_ref(&spec),
            &crate::fixture::key("Patient", "t1"),
        )
        .await
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].url, "urn:p:band");
    assert_eq!(reports[0].indexed, 1);
    assert_eq!(reports[0].values, 1);
    assert!(reports[0].failures.is_empty());

    assert!(store
        .search(&banded("high"))
        .await
        .unwrap()
        .entries
        .is_empty());
    let mut low = found(store.search(&banded("low")).await.unwrap());
    low.sort();
    assert_eq!(low, vec!["t1", "t2"]);

    let other = crate::fixture::observation("t3", "code-1", 3.0, "Patient/t1");
    store.create(other).await.unwrap();
    let untouched = store
        .reindex_resource(
            std::slice::from_ref(&spec),
            &crate::fixture::key("Observation", "t3"),
        )
        .await
        .unwrap();
    assert_eq!(untouched[0].indexed, 0);
    assert_eq!(
        found(store.search(&banded("low")).await.unwrap()).len(),
        2,
        "a resource outside the parameter changed the index"
    );

    let missing = store
        .reindex_resource(
            std::slice::from_ref(&spec),
            &crate::fixture::key("Patient", "nobody"),
        )
        .await;
    assert!(
        matches!(missing, Err(fhir_core::Error::NotFound)),
        "{missing:?}"
    );
}




pub async fn exempted(store: &dyn fhir_store::ResourceStore) {
    let dated = |id: &str, effective: &str| {
        envelope(
            "Observation",
            id,
            &format!(
                r#""status":"final","code":{{"text":"probe"}},"effectiveDateTime":"{effective}""#
            ),
        )
    };
    store.create(dated("x-dated", "2020-01-01")).await.unwrap();
    store.create(dated("x-later", "2024-06-01")).await.unwrap();
    store
        .create(patient("x-plain", "Stone", true))
        .await
        .unwrap();

    let windowed = |exempt: Vec<ResourceType>| SearchQuery {
        types: vec![kind("Observation"), kind("Patient")],
        filters: vec![
            qualified("Observation", "date", Modifier::None, "ge2023-01-01").exempting(exempt),
        ],
        ..SearchQuery::default()
    };

    let mut narrowed = ids(&store.search(&windowed(Vec::new())).await.unwrap());
    narrowed.sort();
    assert_eq!(narrowed, vec!["x-later".to_owned()]);

    let mut carried = ids(&store
        .search(&windowed(vec![kind("Patient")]))
        .await
        .unwrap());
    carried.sort();
    assert_eq!(
        carried,
        vec!["x-later".to_owned(), "x-plain".to_owned()],
        "the exempt type is carried and the judged one is still narrowed"
    );
}






pub async fn converted_quantities(store: &dyn fhir_store::ResourceStore) {
    let weighed = |id: &str, value: f64, code: &str| {
        envelope(
            "Observation",
            id,
            &format!(
                r#""status":"final","code":{{"coding":[{{"system":"urn:s","code":"mass"}}]}},"valueQuantity":{{"value":{value},"system":"http://unitsofmeasure.org","code":"{code}"}}"#
            ),
        )
    };
    store.create(weighed("q-kg", 2.0, "kg")).await.unwrap();
    store.create(weighed("q-g", 2000.0, "g")).await.unwrap();
    store.create(weighed("q-m", 2.0, "m")).await.unwrap();

    let asked = |raw: &str| SearchQuery {
        filters: vec![qualified(
            "Observation",
            "value-quantity",
            Modifier::None,
            raw,
        )],
        ..SearchQuery::of_type(kind("Observation"))
    };

    let mut held = ids(&store
        .search(&asked("2|http://unitsofmeasure.org|kg"))
        .await
        .unwrap());
    held.sort();
    assert_eq!(
        held,
        vec!["q-g".to_owned(), "q-kg".to_owned()],
        "kilograms finds the value recorded in grams"
    );

    let mut held = ids(&store
        .search(&asked("2000|http://unitsofmeasure.org|g"))
        .await
        .unwrap());
    held.sort();
    assert_eq!(held, vec!["q-g".to_owned(), "q-kg".to_owned()]);

    let held = ids(&store
        .search(&asked("2|http://unitsofmeasure.org|m"))
        .await
        .unwrap());
    assert_eq!(
        held,
        vec!["q-m".to_owned()],
        "and finds nothing of another dimension"
    );

    let held = ids(&store
        .search(&asked("gt1|http://unitsofmeasure.org|kg"))
        .await
        .unwrap());
    assert_eq!(held.len(), 2, "a comparator holds across the conversion");
}







pub async fn narrowed_everywhere(store: &dyn fhir_store::ResourceStore) {
    let observed = |id: &str, tenant: &str| {
        crate::fixture::secured(
            "Observation",
            id,
            r#""status":"final","code":{"text":"probe"}"#,
            "urn:t",
            tenant,
        )
    };
    store.create(observed("t-one-ob", "one")).await.unwrap();
    store.create(observed("t-two-ob", "two")).await.unwrap();
    store
        .create(crate::fixture::secured(
            "Patient",
            "t-one-pt",
            r#""active":true"#,
            "urn:t",
            "one",
        ))
        .await
        .unwrap();
    store
        .create(envelope("Patient", "t-none-pt", r#""active":true"#))
        .await
        .unwrap();

    let confined = |tenant: &str| SearchQuery {
        types: vec![kind("Observation"), kind("Patient")],
        grant: Some(Grant {
            types: Vec::new(),
            compartments: Vec::new(),
            filters: Vec::new(),
            every: vec![filter(
                "Observation",
                "_security",
                &format!("urn:t|{tenant}"),
            )],
        }),
        ..SearchQuery::default()
    };

    let mut one = ids(&store.search(&confined("one")).await.unwrap());
    one.sort();
    assert_eq!(
        one,
        vec!["t-one-ob".to_owned(), "t-one-pt".to_owned()],
        "every type is narrowed by the one filter, and a row carrying no label \
         is not reached"
    );

    let mut two = ids(&store.search(&confined("two")).await.unwrap());
    two.sort();
    assert_eq!(two, vec!["t-two-ob".to_owned()]);
}
