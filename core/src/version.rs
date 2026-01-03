use crate::Error;
use crate::resource_id::validate_id_chars;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionId(String);

impl VersionId {
    pub fn parse(value: &str) -> Result<VersionId, Error> {
        validate_id_chars(value).map_err(|_| Error::InvalidVersion(value.to_owned()))?;
        Ok(VersionId(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for VersionId {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> {
        VersionId::parse(value)
    }
}

impl FromStr for VersionId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        VersionId::parse(value)
    }
}

impl fmt::Display for VersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_versions() {
        let v = VersionId::parse("3").expect("valid version should parse");
        assert_eq!(v.as_str(), "3");
    }

    #[test]
    fn rejects_empty_version() {
        assert!(matches!(VersionId::parse(""), Err(Error::InvalidVersion(_))));
    }

    #[test]
    fn rejects_invalid_version_characters() {
        for value in ["1:0", "with space", "v/2"] {
            assert!(matches!(VersionId::parse(value), Err(Error::InvalidVersion(_))));
        }
    }

    #[test]
    fn display_round_trips() {
        let v = VersionId::parse("7").unwrap();
        assert_eq!(v.to_string(), "7");
    }
}