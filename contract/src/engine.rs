pub const ENV_SKIP: &str = "FHIR_TEST_SKIP_ENGINES";

pub fn permits(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true") | Some("yes"))
}

pub fn refusal(engine: &str, url: &str, cause: &str) -> String {
    format!(
        "the {engine} engine at {url} did not answer: {cause}; \
         a run that means to skip the tests needing it sets {ENV_SKIP}=1"
    )
}

pub fn absent<T>(engine: &str, url: &str, cause: &str) -> Option<T> {
    let told = refusal(engine, url, cause);
    match permits(std::env::var(ENV_SKIP).ok().as_deref()) {
        true => {
            eprintln!("skipped: {told}");
            None
        }
        false => panic!("{told}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_named_permission_lets_a_run_skip_an_engine() {
        assert!(permits(Some("1")));
        assert!(permits(Some("true")));
        assert!(permits(Some("yes")));
        assert!(!permits(None));
        assert!(!permits(Some("")));
        assert!(!permits(Some("0")));
        assert!(!permits(Some("no")));
    }

    #[test]
    fn the_refusal_names_the_engine_its_address_and_the_permission() {
        let told = refusal(
            "relational",
            "postgres://127.0.0.1:5432/fhir",
            "connection refused",
        );
        assert!(told.contains("relational"), "{told}");
        assert!(told.contains("postgres://127.0.0.1:5432/fhir"), "{told}");
        assert!(told.contains("connection refused"), "{told}");
        assert!(told.contains(ENV_SKIP), "{told}");
    }
}
