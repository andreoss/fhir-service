mod support;

use std::path::PathBuf;

fn artefact(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{name} is present"))
}

fn tags(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("image:"))
        .map(|value| value.trim().to_owned())
        .collect()
}

#[test]
fn every_image_names_the_version_it_pins() {
    let compose = artefact("compose.yaml");
    let found = tags(&compose);
    assert!(!found.is_empty());
    for tag in &found {
        assert!(tag.contains(':'), "{tag}");
        assert!(!tag.ends_with(":latest"), "{tag}");
    }
}

#[test]
fn the_image_builds_the_workspace_and_carries_the_commands() {
    let text = artefact("Containerfile");
    assert!(text.contains("--locked"), "{text}");
    assert!(text.contains("--release"), "{text}");
    for command in ["fhir-host", "apply", "load", "dump", "reindex", "probe"] {
        assert!(text.contains(command), "{command} is missing");
    }
    assert!(text.contains("ARG "), "{text}");
    for line in text.lines() {
        let line = line.trim();
        if let Some(reference) = line.strip_prefix("FROM ") {
            assert!(
                reference.contains(':') || reference.contains("${"),
                "{line}"
            );
            assert!(!reference.contains(":latest"), "{line}");
        }
    }
}

#[test]
fn one_command_brings_the_service_up_beside_its_engines() {
    let compose = artefact("compose.yaml");
    assert!(compose.contains("service:"), "{compose}");
    assert!(compose.contains("Containerfile"), "{compose}");
    assert!(compose.contains("service_healthy"), "{compose}");
    assert!(compose.contains("healthcheck:"), "{compose}");
    assert!(compose.contains("${"), "{compose}");
}

#[test]
fn no_deployment_artefact_carries_a_secret() {
    for name in ["compose.yaml", "Containerfile"] {
        let text = artefact(name).to_ascii_lowercase();
        for marker in ["private key", "begin rsa", "authorization: bearer"] {
            assert!(!text.contains(marker), "{name} carries {marker}");
        }
    }
}

fn parameters() -> Vec<String> {
    artefact("deploy/parameters.env")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
        .collect()
}

#[test]
fn the_cluster_template_varies_only_through_its_parameters() {
    let template = artefact("deploy/cluster.yaml");
    let declared = parameters();
    assert!(!declared.is_empty());

    let mut used: Vec<String> = Vec::new();
    let mut rest = template.as_str();
    while let Some(at) = rest.find("${") {
        rest = &rest[at + 2..];
        let end = rest.find('}').expect("a parameter closes");
        used.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    assert!(!used.is_empty());
    for name in &used {
        assert!(declared.contains(name), "{name} is not declared");
    }
    for name in [
        "FHIR_IMAGE",
        "FHIR_REPLICAS",
        "FHIR_CONTINUATION_KEY",
        "FHIR_STORE_CONNECTIONS",
    ] {
        assert!(
            declared.contains(&name.to_owned()),
            "{name} is not declared"
        );
    }
}

#[test]
fn the_cluster_template_shares_what_instances_must_share() {
    let template = artefact("deploy/cluster.yaml");
    assert!(template.contains("${FHIR_REPLICAS}"), "{template}");
    assert!(template.contains("FHIR_CONTINUATION_KEY"), "{template}");
    assert!(template.contains("FHIR_STORE_CONNECTIONS"), "{template}");
    assert!(template.contains("secretKeyRef"), "{template}");
    assert!(!template.contains(":latest"), "{template}");
}

#[test]
fn no_cluster_parameter_carries_a_value_that_must_stay_secret() {
    let declared = artefact("deploy/parameters.env");
    for line in declared.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = line.split_once('=').expect("a parameter carries a default");
        if name.contains("KEY") || name.contains("PASSWORD") || name.contains("CREDENTIAL") {
            assert!(value.trim().is_empty(), "{name} carries a value");
        }
    }
}

#[test]
fn the_probe_reports_what_it_takes_and_refuses_a_closed_address() {
    let (code, text) = support::tool(env!("CARGO_BIN_EXE_probe"), &[], "fhir");
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("usage"), "{text}");

    let (code, text) = support::tool(env!("CARGO_BIN_EXE_probe"), &["127.0.0.1:1"], "fhir");
    assert_eq!(code, 1, "{text}");
}

#[test]
fn the_probe_answers_for_a_running_instance() {
    let Some(binary) = support::service_binary() else {
        return;
    };
    let (code, text) = support::tool_with(
        env!("CARGO_BIN_EXE_instances"),
        &["1", binary.to_str().expect("a usable path")],
        "fhir",
        &[("FHIR_BACKEND", "memory")],
    );
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"status\":200"), "{text}");
}
