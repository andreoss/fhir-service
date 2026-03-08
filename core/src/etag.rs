use crate::Error;
use crate::VersionId;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WeakEtag(String);

impl WeakEtag {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<VersionId> for WeakEtag {
    fn from(version: VersionId) -> Self {
        WeakEtag(version.as_str().to_owned())
    }
}

impl From<&VersionId> for WeakEtag {
    fn from(version: &VersionId) -> Self {
        WeakEtag(version.as_str().to_owned())
    }
}

impl FromStr for WeakEtag {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        let inner = value
            .strip_prefix("W/\"")
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(|| Error::InvalidEtag(value.to_owned()))?;
        if inner.is_empty() {
            return Err(Error::InvalidEtag(value.to_owned()));
        }
        Ok(WeakEtag(inner.to_owned()))
    }
}

impl TryFrom<&str> for WeakEtag {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> {
        WeakEtag::from_str(value)
    }
}

impl fmt::Display for WeakEtag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "W/\"{}\"", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_weak_etag_from_version() {
        let version = VersionId::parse("4").unwrap();
        assert_eq!(WeakEtag::from(&version).to_string(), "W/\"4\"");
        assert_eq!(WeakEtag::from(version).as_str(), "4");
    }

    #[test]
    fn parses_full_etag_header_value() {
        let etag: WeakEtag = "W/\"9\"".parse().unwrap();
        assert_eq!(etag.to_string(), "W/\"9\"");
    }

    #[test]
    fn rejects_malformed_etags() {
        for value in ["\"9\"", "W/", "W/\"\"", "9", "W/\"1\"x"] {
            assert!(matches!(
                value.parse::<WeakEtag>(),
                Err(Error::InvalidEtag(_))
            ));
        }
    }

    #[test]
    fn rejects_empty_inner_value() {
        assert!(matches!(
            "W/\"\"".parse::<WeakEtag>(),
            Err(Error::InvalidEtag(_))
        ));
    }
}
