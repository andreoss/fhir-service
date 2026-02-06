use crate::Error;
use std::str::FromStr;

const LENGTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CorrelationId(String);

impl CorrelationId {
    pub fn parse(raw: &str) -> Result<CorrelationId, Error> {
        let valid = raw.len() == LENGTH
            && raw
                .chars()
                .all(|letter| letter.is_ascii_digit() || ('a'..='f').contains(&letter));
        match valid {
            true => Ok(CorrelationId(raw.to_owned())),
            false => Err(Error::InvalidParameter(
                "a correlation identifier is thirty-two hexadecimal digits".to_owned(),
            )),
        }
    }

    pub fn offered(raw: Option<&str>) -> CorrelationId {
        raw.and_then(|held| CorrelationId::parse(held).ok())
            .unwrap_or_else(CorrelationId::fresh)
    }

    pub fn fresh() -> CorrelationId {
        CorrelationId(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CorrelationId {
    type Err = Error;

    fn from_str(raw: &str) -> Result<CorrelationId, Error> {
        CorrelationId::parse(raw)
    }
}

impl std::fmt::Display for CorrelationId {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drawn_identifier_parses_back() {
        let drawn = CorrelationId::fresh();
        assert_eq!(CorrelationId::parse(drawn.as_str()).unwrap(), drawn);
        assert_eq!(drawn.as_str().len(), LENGTH);
    }

    #[test]
    fn two_drawn_identifiers_differ() {
        assert_ne!(CorrelationId::fresh(), CorrelationId::fresh());
    }

    #[test]
    fn text_that_is_not_of_the_shape_is_refused() {
        for raw in [
            "",
            "pt-confidential-77",
            "Rossignol",
            "ABCDEF0123456789abcdef0123456789",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdefa",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(CorrelationId::parse(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn an_offered_identifier_of_another_shape_is_replaced() {
        let offered = CorrelationId::offered(Some("patient-smith-1"));
        assert_ne!(offered.as_str(), "patient-smith-1");
        assert_eq!(offered.as_str().len(), LENGTH);
        let drawn = CorrelationId::fresh();
        assert_eq!(CorrelationId::offered(Some(drawn.as_str())), drawn);
        assert_eq!(CorrelationId::offered(None).as_str().len(), LENGTH);
    }
}
