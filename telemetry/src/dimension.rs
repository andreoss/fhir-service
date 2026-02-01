#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operation {
    Read,
    Vread,
    Create,
    Update,
    Delete,
    Patch,
    Search,
    History,
    Bundle,
    Conformance,
    Export,
    Import,
    Reindex,
    BulkDelete,
    BulkUpdate,
    Job,
    Other,
}

impl Operation {
    pub const ALL: [Operation; 17] = [
        Operation::Read,
        Operation::Vread,
        Operation::Create,
        Operation::Update,
        Operation::Delete,
        Operation::Patch,
        Operation::Search,
        Operation::History,
        Operation::Bundle,
        Operation::Conformance,
        Operation::Export,
        Operation::Import,
        Operation::Reindex,
        Operation::BulkDelete,
        Operation::BulkUpdate,
        Operation::Job,
        Operation::Other,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::Read => "read",
            Operation::Vread => "vread",
            Operation::Create => "create",
            Operation::Update => "update",
            Operation::Delete => "delete",
            Operation::Patch => "patch",
            Operation::Search => "search",
            Operation::History => "history",
            Operation::Bundle => "bundle",
            Operation::Conformance => "conformance",
            Operation::Export => "export",
            Operation::Import => "import",
            Operation::Reindex => "reindex",
            Operation::BulkDelete => "bulk_delete",
            Operation::BulkUpdate => "bulk_update",
            Operation::Job => "job",
            Operation::Other => "other",
        }
    }

    pub fn slot(&self) -> usize {
        Operation::ALL
            .iter()
            .position(|held| held == self)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Outcome {
    Success,
    ClientFault,
    ServerFault,
}

impl Outcome {
    pub const ALL: [Outcome; 3] = [Outcome::Success, Outcome::ClientFault, Outcome::ServerFault];

    pub fn of_status(status: u16) -> Outcome {
        match status {
            0..=399 => Outcome::Success,
            400..=499 => Outcome::ClientFault,
            _ => Outcome::ServerFault,
        }
    }

    pub fn is_failure(&self) -> bool {
        match self {
            Outcome::Success => false,
            Outcome::ClientFault | Outcome::ServerFault => true,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::ClientFault => "client_fault",
            Outcome::ServerFault => "server_fault",
        }
    }

    pub fn slot(&self) -> usize {
        Outcome::ALL
            .iter()
            .position(|held| held == self)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimensions {
    pub operation: Operation,
    pub outcome: Outcome,
}

impl Dimensions {
    pub const COUNT: usize = Operation::ALL.len() * Outcome::ALL.len();

    pub fn of(operation: Operation, outcome: Outcome) -> Dimensions {
        Dimensions { operation, outcome }
    }

    pub fn slot(&self) -> usize {
        self.operation.slot() * Outcome::ALL.len() + self.outcome.slot()
    }

    pub fn at(slot: usize) -> Dimensions {
        Dimensions {
            operation: Operation::ALL[slot / Outcome::ALL.len()],
            outcome: Outcome::ALL[slot % Outcome::ALL.len()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_names_the_label_set_it_came_from() {
        for slot in 0..Dimensions::COUNT {
            assert_eq!(Dimensions::at(slot).slot(), slot);
        }
    }

    #[test]
    fn a_refusal_and_a_fault_both_count_as_failures() {
        assert!(!Outcome::Success.is_failure());
        assert!(Outcome::ClientFault.is_failure());
        assert!(Outcome::ServerFault.is_failure());
    }

    #[test]
    fn every_operation_has_a_distinct_name() {
        let mut names: Vec<&str> = Operation::ALL.iter().map(|held| held.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Operation::ALL.len());
    }
}
