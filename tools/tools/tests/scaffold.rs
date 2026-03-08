mod support;

use std::path::Path;

fn generated(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_else(|_| panic!("{name} is written"))
}

fn commented(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("//") && !trimmed.starts_with("///")
    })
}

#[test]
fn the_scaffolder_reports_what_it_takes() {
    let (code, text) = support::tool(env!("CARGO_BIN_EXE_scaffold"), &[], "fhir");
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("usage"), "{text}");
}

#[test]
fn a_scaffolded_job_carries_a_handler_a_test_and_its_registration() {
    let root = support::scratch("scaffold");
    let (code, text) = support::tool(
        env!("CARGO_BIN_EXE_scaffold"),
        &["Cleanup", "reindex", root.to_str().expect("a usable path")],
        "fhir",
    );
    assert_eq!(code, 0, "{text}");

    let handler = generated(&root, "cleanup.rs");
    assert!(handler.contains("pub struct CleanupJob"), "{handler}");
    assert!(
        handler.contains("impl JobHandler for CleanupJob"),
        "{handler}"
    );
    assert!(handler.contains("fn kind(&self) -> JobKind"), "{handler}");
    assert!(handler.contains("JobKind::Reindex"), "{handler}");
    assert!(handler.contains("async fn plan"), "{handler}");
    assert!(handler.contains("async fn process"), "{handler}");
    assert!(!commented(&handler), "{handler}");

    let test = generated(&root, "cleanup_test.rs");
    assert!(test.contains("#[tokio::test]"), "{test}");
    assert!(test.contains("CleanupJob"), "{test}");
    assert!(!commented(&test), "{test}");

    assert!(text.contains("pub mod cleanup;"), "{text}");
    assert!(text.contains("CleanupJob"), "{text}");

    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
}

#[test]
fn an_unknown_kind_and_an_unusable_name_are_refused() {
    let root = support::scratch("refused");
    let path = root.to_str().expect("a usable path").to_owned();
    for args in [
        ["Cleanup", "nonsense", path.as_str()],
        ["not a name", "reindex", path.as_str()],
        ["", "reindex", path.as_str()],
    ] {
        let (code, text) = support::tool(env!("CARGO_BIN_EXE_scaffold"), &args, "fhir");
        assert_eq!(code, 1, "{text}");
    }
    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
}

#[test]
fn a_scaffold_never_writes_over_what_is_there() {
    let root = support::scratch("twice");
    let path = root.to_str().expect("a usable path").to_owned();
    let args = ["Cleanup", "import", path.as_str()];
    let (code, text) = support::tool(env!("CARGO_BIN_EXE_scaffold"), &args, "fhir");
    assert_eq!(code, 0, "{text}");
    let (code, text) = support::tool(env!("CARGO_BIN_EXE_scaffold"), &args, "fhir");
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("cleanup.rs"), "{text}");
    std::fs::remove_dir_all(&root).expect("the scratch directory is removed");
}
