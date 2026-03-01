use axum::http::header::{HeaderMap, IF_MODIFIED_SINCE, IF_NONE_MATCH};
use fhir_core::{ResourceEnvelope, VersionId};
use time::format_description::well_known::Rfc2822;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    Any,
    Version(VersionId),
    Since(i64),
}

pub fn asked_for(headers: &HeaderMap) -> Option<Precondition> {
    match headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        Some(text) => match parse_etag(text.trim()) {
            Some(Precondition::Any) => Some(Precondition::Any),
            Some(precondition) => Some(precondition),
            None => since(headers),
        },
        None => since(headers),
    }
}

fn parse_etag(text: &str) -> Option<Precondition> {
    if text == "*" {
        return Some(Precondition::Any);
    }
    let version = text
        .trim()
        .strip_prefix("W/")
        .unwrap_or(text)
        .trim_matches('"');
    version.parse::<VersionId>().ok().map(Precondition::Version)
}

fn since(headers: &HeaderMap) -> Option<Precondition> {
    let text = headers.get(IF_MODIFIED_SINCE)?.to_str().ok()?;
    OffsetDateTime::parse(text, &Rfc2822)
        .ok()
        .map(|instant| Precondition::Since(instant.unix_timestamp()))
}

pub fn holds(precondition: Option<&Precondition>, envelope: &ResourceEnvelope) -> bool {
    match precondition {
        Some(Precondition::Any) => true,
        Some(Precondition::Version(version)) => version == envelope.version_id(),
        Some(Precondition::Since(seconds)) => envelope.last_updated().key().seconds() <= *seconds,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{asked_for, holds, Precondition};
    use axum::http::header::{HeaderMap, IF_MODIFIED_SINCE, IF_NONE_MATCH};
    use fhir_core::{FhirInstant, FhirVersion, ResourceEnvelope, VersionId};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, value.parse().expect("header value"));
        }
        headers
    }

    fn envelope(version: &str, written: &str) -> ResourceEnvelope {
        ResourceEnvelope::from_metadata(
            FhirVersion::R4,
            "Patient".parse().expect("type"),
            "p1".parse().expect("id"),
            version.parse::<VersionId>().expect("version"),
            FhirInstant::parse(written).expect("instant"),
        )
    }

    #[test]
    fn a_wildcard_asks_for_any_version() {
        let headers = headers(&[(IF_NONE_MATCH.as_str(), "*")]);

        assert_eq!(asked_for(&headers), Some(Precondition::Any));
        assert!(holds(
            asked_for(&headers).as_ref(),
            &envelope("3", "2026-09-06T04:00:00.000Z")
        ));
    }

    #[test]
    fn a_weak_etag_asks_for_its_version() {
        let headers = headers(&[(IF_NONE_MATCH.as_str(), "W/\"3\"")]);

        assert_eq!(
            asked_for(&headers),
            Some(Precondition::Version("3".parse().expect("version")))
        );
    }

    #[test]
    fn a_read_since_a_write_asks_for_the_time() {
        let headers = headers(&[(IF_MODIFIED_SINCE.as_str(), "Sun, 06 Sep 2026 04:00:00 GMT")]);

        assert_eq!(
            asked_for(&headers),
            Some(Precondition::Since(1_788_667_200))
        );
    }

    #[test]
    fn a_version_that_is_held_is_unchanged_and_one_that_is_not_is_changed() {
        let held = envelope("2", "2026-09-06T04:00:00.000Z");

        assert!(holds(
            Some(&Precondition::Version("2".parse().expect("version"))),
            &held
        ));
        assert!(!holds(
            Some(&Precondition::Version("1".parse().expect("version"))),
            &held
        ));
        assert!(holds(Some(&Precondition::Since(1_788_667_200)), &held));
        assert!(!holds(Some(&Precondition::Since(1_788_667_199)), &held));
    }

    #[test]
    fn no_precondition_is_never_unchanged() {
        assert!(!holds(None, &envelope("1", "2026-09-06T04:00:00.000Z")));
        assert_eq!(asked_for(&HeaderMap::new()), None);
    }
}
