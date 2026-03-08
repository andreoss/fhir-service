use fhir_core::Error;

const LIMIT: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace(String);

impl Namespace {
    pub fn parse(raw: &str) -> Result<Namespace, Error> {
        let valid = !raw.is_empty()
            && raw.len() <= LIMIT
            && raw.starts_with(|first: char| first.is_ascii_lowercase())
            && raw.chars().all(|letter| {
                letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '_'
            });
        match valid {
            true => Ok(Namespace(raw.to_owned())),
            false => Err(Error::Config(format!(
                "invalid namespace name {raw:?}; expected lowercase letters, digits and underscores"
            ))),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Namespace {
    fn default() -> Namespace {
        Namespace("fhir".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_is_accepted() {
        assert_eq!(Namespace::parse("fhir_main").unwrap().as_str(), "fhir_main");
    }

    #[test]
    fn the_default_names_one_space() {
        assert_eq!(Namespace::default().as_str(), "fhir");
    }

    #[test]
    fn a_name_carrying_a_quote_is_refused() {
        for raw in [
            "",
            "Public",
            "1abc",
            "a b",
            "a\"b",
            "a;drop",
            &"a".repeat(LIMIT + 1),
        ] {
            assert!(Namespace::parse(raw).is_err(), "{raw:?} should be refused");
        }
    }
}
