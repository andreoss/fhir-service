mod support;

const ROWS: &str = concat!(
    "{\"resourceType\":\"Patient\",\"id\":\"tool-one\"}\n",
    "{\"resourceType\":\"Patient\",\"id\":\"tool-two\"}\n"
);

#[test]
fn each_command_reports_what_it_takes() {
    for binary in [
        env!("CARGO_BIN_EXE_load"),
        env!("CARGO_BIN_EXE_dump"),
        env!("CARGO_BIN_EXE_reindex"),
    ] {
        let (code, text) = support::tool(binary, &[], "fhir");
        assert_eq!(code, 2, "{text}");
        assert!(text.contains("usage"), "{text}");
    }
}

#[tokio::test]
async fn a_supply_loads_and_leaves_the_service_out_of_it() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("load");
    support::prepared(&pool, &namespace).await;
    let root = support::scratch("load");
    let supply = root.join("supply.ndjson");
    std::fs::write(&supply, ROWS).expect("the supply is written");

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_load"),
        &[supply.to_str().expect("a usable path")],
        namespace.as_str(),
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"handled\":2"), "{text}");

    let out = root.join("out");
    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_dump"),
        &[out.to_str().expect("a usable path"), "Patient"],
        namespace.as_str(),
    );
    assert_eq!(code, 0, "{text}");
    let written =
        std::fs::read_to_string(out.join("Patient.ndjson")).expect("the export is written");
    assert_eq!(
        written
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count(),
        2,
        "{written}"
    );

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_reindex"),
        &["Patient"],
        namespace.as_str(),
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"handled\""), "{text}");

    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_row_that_cannot_be_loaded_is_reported_by_position() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("broken");
    support::prepared(&pool, &namespace).await;
    let root = support::scratch("broken");
    let supply = root.join("supply.ndjson");
    std::fs::write(
        &supply,
        "{\"resourceType\":\"Patient\",\"id\":\"ok\"}\nnot a resource\n",
    )
    .expect("the supply is written");

    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_load"),
        &[supply.to_str().expect("a usable path")],
        namespace.as_str(),
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("row 1"), "{text}");

    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
    support::drop_namespace(&pool, &namespace).await;
}

#[test]
fn a_missing_supply_fails_without_touching_the_store() {
    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_load"),
        &["scratch/absent.ndjson"],
        "fhir",
    );
    assert_eq!(code, 1, "{text}");
}
