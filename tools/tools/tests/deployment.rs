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

#[test]
fn the_image_copies_only_what_the_workspace_holds() {
    let text = artefact("Containerfile");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let mut copied = 0;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("COPY ") else {
            continue;
        };
        if rest.starts_with("--from") {
            continue;
        }
        let mut sources: Vec<&str> = rest.split_whitespace().collect();
        sources.pop();
        for source in sources {
            assert!(
                root.join(source).exists(),
                "the workspace holds no {source}"
            );
            copied += 1;
        }
    }
    assert!(copied > 0, "{text}");
}

#[test]
fn the_conformance_stage_stands_up_what_it_judges() {
    let script = artefact("ci/conformance.sh");
    for named in [
        "smart-app-launch-test-kit",
        "FHIR_CONFORMANCE_REVISION:-v",
        "--profile conformance",
        "--build",
        "inferno migrate",
        "inferno execute",
    ] {
        assert!(script.contains(named), "the stage does not carry {named}");
    }
    let compose = artefact("compose.yaml");
    for named in [
        "issuer:",
        "realm.json",
        "\"conformance\"",
        "https-certificate-file",
    ] {
        assert!(
            compose.contains(named),
            "the compose file does not carry {named}"
        );
    }
}

#[test]
fn the_realm_carries_what_the_preset_asks_of_it() {
    let realm: serde_json::Value =
        serde_json::from_str(&artefact("ci/conformance/realm.json")).expect("the realm is json");
    let preset: serde_json::Value =
        serde_json::from_str(&artefact("ci/conformance/preset.json")).expect("the preset is json");
    let clients: Vec<&str> = realm["clients"]
        .as_array()
        .expect("the realm holds clients")
        .iter()
        .filter_map(|client| client["clientId"].as_str())
        .collect();
    let scopes: Vec<&str> = realm["clientScopes"]
        .as_array()
        .expect("the realm holds scopes")
        .iter()
        .filter_map(|scope| scope["name"].as_str())
        .collect();
    let inputs = preset["inputs"]
        .as_array()
        .expect("the preset holds inputs");
    let mut named = 0;
    for input in inputs {
        let Some(client) = input["value"]["client_id"].as_str() else {
            continue;
        };
        assert!(clients.contains(&client), "the realm holds no {client}");
        named += 1;
        let requested = input["value"]["requested_scopes"]
            .as_str()
            .unwrap_or_default();
        for scope in requested.split_whitespace() {
            if scope == "openid" {
                continue;
            }
            assert!(scopes.contains(&scope), "the realm publishes no {scope}");
        }
    }
    assert!(named > 0, "the preset names no client");
}

#[test]
fn the_realm_holds_only_the_keys_the_kit_signs_with_publicly() {
    let realm: serde_json::Value =
        serde_json::from_str(&artefact("ci/conformance/realm.json")).expect("the realm is json");
    let backend = realm["clients"]
        .as_array()
        .expect("the realm holds clients")
        .iter()
        .find(|client| client["attributes"]["use.jwks.string"] == "true")
        .expect("a client the kit authenticates as");
    let held = backend["attributes"]["jwks.string"]
        .as_str()
        .expect("the client holds a key set");
    let keys: serde_json::Value = serde_json::from_str(held).expect("the key set is json");
    let keys = keys["keys"].as_array().expect("the key set holds keys");
    assert!(!keys.is_empty());
    for key in keys {
        assert!(key.get("d").is_none(), "a private key is committed: {key}");
        assert_eq!(key["key_ops"][0], "verify", "{key}");
    }
}
