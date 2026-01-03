use std::process::Command;

#[test]
fn invalid_backend_fails_fast_at_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "nosql")
        .output()
        .expect("failed to spawn binary");
    assert!(!output.status.success(), "invalid config must exit non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FHIR_BACKEND"), "stderr was: {stderr}");
}

#[test]
fn unimplemented_backend_fails_fast_at_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "relational")
        .output()
        .expect("failed to spawn binary");
    assert!(!output.status.success(), "unimplemented backend must exit non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FHIR_BACKEND"), "stderr was: {stderr}");
}

#[test]
fn feature_gated_backend_rejected_when_not_built() {
    let output = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "document")
        .env("FHIR_DATA_DIR", "/var/lib/fhir")
        .output()
        .expect("failed to spawn binary");
    assert!(!output.status.success(), "a backend absent from the build must exit non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("backend-document"), "stderr was: {stderr}");
}

#[test]
fn invalid_fhir_version_fails_fast_at_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_VERSION", "2")
        .output()
        .expect("failed to spawn binary");
    assert!(!output.status.success(), "invalid version must exit non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FHIR_VERSION"), "stderr was: {stderr}");
}

#[test]
fn invalid_bind_fails_fast_at_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BIND", "not-an-address")
        .output()
        .expect("failed to spawn binary");
    assert!(!output.status.success(), "invalid bind must exit non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FHIR_BIND"), "stderr was: {stderr}");
}

#[test]
fn document_backend_without_data_dir_fails_fast_at_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_fhir-host"))
        .env("FHIR_BACKEND", "document")
        .output()
        .expect("failed to spawn binary");
    assert!(!output.status.success(), "document backend without a data dir must exit non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FHIR_DATA_DIR"), "stderr was: {stderr}");
}