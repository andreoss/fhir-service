use crate::range::FeedRange;
use async_trait::async_trait;
use fhir_core::{Error, FhirInstant, ResourceId, ResourceType, VersionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Updated,
    Deleted,
}

impl ChangeKind {
    pub fn of(version: &VersionId, deleted: bool) -> ChangeKind {
        if deleted {
            return ChangeKind::Deleted;
        }
        match version.as_str() == "1" {
            true => ChangeKind::Created,
            false => ChangeKind::Updated,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeKind::Created => "created",
            ChangeKind::Updated => "updated",
            ChangeKind::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRecord {
    pub sequence: i64,
    pub resource_type: ResourceType,
    pub id: ResourceId,
    pub version: VersionId,
    pub kind: ChangeKind,
    pub last_updated: FhirInstant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Continuation(i64);

impl Continuation {
    pub fn start() -> Continuation {
        Continuation(0)
    }

    pub fn after(sequence: i64) -> Continuation {
        Continuation(sequence)
    }

    pub fn sequence(&self) -> i64 {
        self.0
    }

    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    pub fn parse(raw: &str) -> Result<Continuation, Error> {
        raw.parse::<i64>()
            .map(Continuation)
            .map_err(|_| Error::InvalidParameter(format!("continuation {raw:?} is not a position")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangePage {
    pub records: Vec<ChangeRecord>,
    pub next: Continuation,
}

#[async_trait]
pub trait ChangeFeed: Send + Sync {
    async fn changes(
        &self,
        range: FeedRange,
        after: &Continuation,
        count: usize,
    ) -> Result<ChangePage, Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(raw: &str) -> VersionId {
        VersionId::parse(raw).expect("a version")
    }

    #[test]
    fn the_first_version_of_a_resource_is_a_creation() {
        assert_eq!(ChangeKind::of(&version("1"), false), ChangeKind::Created);
        assert_eq!(ChangeKind::of(&version("2"), false), ChangeKind::Updated);
    }

    #[test]
    fn a_delete_marker_is_a_deletion_whatever_version_it_carries() {
        assert_eq!(ChangeKind::of(&version("2"), true), ChangeKind::Deleted);
        assert_eq!(ChangeKind::of(&version("9"), true), ChangeKind::Deleted);
        assert_eq!(ChangeKind::Deleted.as_str(), "deleted");
        assert_eq!(ChangeKind::Created.as_str(), "created");
        assert_eq!(ChangeKind::Updated.as_str(), "updated");
    }

    #[test]
    fn a_position_survives_being_written_down() {
        let held = Continuation::after(7);
        assert_eq!(Continuation::parse(&held.as_str()).unwrap(), held);
        assert_eq!(Continuation::start().sequence(), 0);
        assert_eq!(Continuation::default(), Continuation::start());
        assert!(Continuation::parse("later").is_err());
    }
}
