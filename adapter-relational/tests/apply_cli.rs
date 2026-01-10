mod support;

use std::process::Command;

fn run(namespace: &str, args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_apply"))
        .args(args)
        .env(fhir_adapter_relational::ENV_URL, support::url())
        .env(fhir_adapter_relational::ENV_NAMESPACE, namespace)
        .output()
        .expect("the apply command runs");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), text)
}

#[test]
fn an_unknown_command_reports_the_ones_it_takes() {
    let (code, text) = run("fhir", &["sideways"]);
    assert_eq!(code, 2);
    assert!(text.contains("version"), "{text}");
    assert!(text.contains("next"), "{text}");
    assert!(text.contains("latest"), "{text}");
    assert!(text.contains("force"), "{text}");
}

#[test]
fn force_without_a_version_is_refused() {
    let (code, text) = run("fhir", &["force"]);
    assert_eq!(code, 2);
    assert!(text.contains("force"), "{text}");
}

#[tokio::test]
async fn the_command_reports_and_advances_the_schema() {
    let Some(pool) = support::engine().await else { return };
    let namespace = support::namespace("cli");
    let name = namespace.as_str().to_owned();

    let (code, text) = run(&name, &["version"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("behind"), "{text}");

    let (code, text) = run(&name, &["next"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains('1'), "{text}");

    let (code, text) = run(&name, &["latest"]);
    assert_eq!(code, 0, "{text}");

    let (code, text) = run(&name, &["version"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("compatible"), "{text}");

    let (code, text) = run(&name, &["force", "1"]);
    assert_eq!(code, 0, "{text}");

    let (code, text) = run(&name, &["force", "99"]);
    assert_eq!(code, 1, "{text}");

    support::drop_namespace(&pool, &namespace).await;
}

#[test]
fn an_invalid_namespace_fails_fast() {
    let (code, text) = run("Not Valid", &["version"]);
    assert_eq!(code, 1);
    assert!(text.contains("schema name"), "{text}");
}
