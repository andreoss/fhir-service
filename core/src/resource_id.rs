use crate::Error;
use std::fmt;
use std::str::FromStr;

pub const MAX_ID_LENGTH: usize = 64;

pub fn validate_id_chars(value: &str) -> Result<(), Error> {
    if value.is_empty() {
        return Err(Error::InvalidResourceId(value.to_owned()));
    }
    if value.len() > MAX_ID_LENGTH {
        return Err(Error::InvalidResourceId(value.to_owned()));
    }
    for ch in value.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '.') {
            return Err(Error::InvalidResourceId(value.to_owned()));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn parse(value: &str) -> Result<ResourceId, Error> {
        validate_id_chars(value)?;
        Ok(ResourceId(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ResourceId {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> {
        ResourceId::parse(value)
    }
}

impl FromStr for ResourceId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        ResourceId::parse(value)
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_ids() {
        for value in ["a", "abc-123.xyz", "1234567890123456789012345678901234567890123456789012345678901234"] {
            let id = ResourceId::parse(value).expect("valid id should parse");
            assert_eq!(id.as_str(), value);
        }
    }

    #[test]
    fn rejects_empty_id() {
        assert!(matches!(ResourceId::parse(""), Err(Error::InvalidResourceId(_))));
    }

    #[test]
    fn rejects_id_longer_than_64() {
        let value = "a".repeat(65);
        assert!(matches!(ResourceId::parse(&value), Err(Error::InvalidResourceId(_))));
    }

    #[test]
    fn rejects_invalid_id_characters() {
        for value in ["with space", "slash/", "under_score", "ok:", "a\nb"] {
            assert!(matches!(ResourceId::parse(value), Err(Error::InvalidResourceId(_))));
        }
    }

    #[test]
    fn display_round_trips() {
        let id = ResourceId::parse("pt-01").unwrap();
        assert_eq!(id.to_string(), "pt-01");
    }
}