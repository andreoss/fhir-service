use crate::Error;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FhirVersion {
    Stu3,
    R4,
    R4b,
    R5,
}

impl FhirVersion {
    pub const ALL: [FhirVersion; 4] = [FhirVersion::Stu3, FhirVersion::R4, FhirVersion::R4b, FhirVersion::R5];

    pub fn as_str(&self) -> &str {
        match self {
            FhirVersion::Stu3 => "STU3",
            FhirVersion::R4 => "R4",
            FhirVersion::R4b => "R4B",
            FhirVersion::R5 => "R5",
        }
    }
}

impl FromStr for FhirVersion {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stu3" | "3" | "3.0.1" => Ok(FhirVersion::Stu3),
            "r4" | "4" | "4.0.1" => Ok(FhirVersion::R4),
            "r4b" | "4b" | "4.3.0" => Ok(FhirVersion::R4b),
            "r5" | "5" | "5.0.0" => Ok(FhirVersion::R5),
            _ => Err(Error::InvalidFhirVersion(value.to_owned())),
        }
    }
}

impl TryFrom<&str> for FhirVersion {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Error> {
        FhirVersion::from_str(value)
    }
}

impl fmt::Display for FhirVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_and_numeric_codes() {
        let cases = [
            (FhirVersion::Stu3, &["STU3", "stu3", "3", "3.0.1"][..]),
            (FhirVersion::R4, &["R4", "r4", "4", "4.0.1"][..]),
            (FhirVersion::R4b, &["R4B", "r4b", "4B", "4.3.0"][..]),
            (FhirVersion::R5, &["R5", "r5", "5", "5.0.0"][..]),
        ];
        for (expected, codes) in cases {
            for code in codes {
                let parsed: FhirVersion = code.parse().expect("valid version code must parse");
                assert_eq!(parsed, expected);
                assert_eq!(parsed.as_str(), expected.as_str());
            }
        }
    }

    #[test]
    fn rejects_unknown_version_codes() {
        for value in ["", "2", "4.1", "FHIR_R4", "R3", "x", "   "] {
            assert!(matches!(value.parse::<FhirVersion>(), Err(Error::InvalidFhirVersion(_))));
        }
    }

    #[test]
    fn all_versions_are_distinct_and_display_round_trips() {
        let mut seen = Vec::new();
        for version in FhirVersion::ALL {
            assert!(!seen.contains(&version), "duplicate variant in ALL");
            seen.push(version);
            assert_eq!(version.to_string().parse::<FhirVersion>().unwrap(), version);
        }
        assert_eq!(seen.len(), 4);
    }
}