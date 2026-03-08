use crate::{Error, ResourceType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataAction {
    Read,
    Write,
    Export,
    Import,
    Reindex,
    BulkDelete,
    BulkUpdate,
    ParameterManagement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Patient,
    User,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub subject: Subject,
    pub target: Option<ResourceType>,
    pub actions: Vec<DataAction>,
    pub filters: Vec<(String, String)>,
}

impl DataAction {
    pub const ALL: [DataAction; 8] = [
        DataAction::Read,
        DataAction::Write,
        DataAction::Export,
        DataAction::Import,
        DataAction::Reindex,
        DataAction::BulkDelete,
        DataAction::BulkUpdate,
        DataAction::ParameterManagement,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            DataAction::Read => "read",
            DataAction::Write => "write",
            DataAction::Export => "export",
            DataAction::Import => "import",
            DataAction::Reindex => "reindex",
            DataAction::BulkDelete => "bulk-delete",
            DataAction::BulkUpdate => "bulk-update",
            DataAction::ParameterManagement => "parameter-management",
        }
    }

    pub fn named(text: &str) -> Option<DataAction> {
        DataAction::ALL
            .into_iter()
            .find(|action| action.as_str() == text)
    }
}

fn lettered(text: &str) -> Option<Vec<DataAction>> {
    let mut actions = Vec::new();
    for letter in text.chars() {
        match letter {
            'r' | 's' => actions.push(DataAction::Read),
            'c' | 'u' | 'd' => actions.push(DataAction::Write),
            _ => return None,
        }
    }
    actions.sort();
    actions.dedup();
    Some(actions)
}

fn pairs(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

impl Subject {
    pub fn named(text: &str) -> Option<Subject> {
        match text {
            "patient" => Some(Subject::Patient),
            "user" => Some(Subject::User),
            "system" => Some(Subject::System),
            _ => None,
        }
    }
}

impl Scope {
    pub fn parse(raw: &str) -> Result<Scope, Error> {
        let refused = || Error::InvalidParameter(format!("scope {raw:?}"));
        let (subject_text, rest) = raw.split_once('/').ok_or_else(refused)?;
        let subject = Subject::named(subject_text.trim()).ok_or_else(refused)?;
        let (rest, query) = match rest.split_once('?') {
            Some((rest, query)) => (rest, query),
            None => (rest, ""),
        };
        let (target_text, action_text) = rest.split_once('.').ok_or_else(refused)?;
        let target = match target_text.trim() {
            "*" => None,
            named => Some(named.parse::<ResourceType>()?),
        };
        let actions = match action_text.trim() {
            "*" => vec![DataAction::Read, DataAction::Write],
            named => match DataAction::named(named) {
                Some(action) => vec![action],
                None => lettered(named).ok_or_else(refused)?,
            },
        };
        if actions.is_empty() {
            return Err(refused());
        }
        Ok(Scope {
            subject,
            target,
            actions,
            filters: pairs(query),
        })
    }

    pub fn parse_all(raw: &[String]) -> Vec<Scope> {
        raw.iter()
            .filter_map(|entry| Scope::parse(entry).ok())
            .collect()
    }

    pub fn permits(&self, action: DataAction, resource_type: Option<ResourceType>) -> bool {
        if !self.actions.contains(&action) {
            return false;
        }
        match (self.target, resource_type) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(granted), Some(asked)) => granted == asked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a known type")
    }

    #[test]
    fn a_scope_names_its_subject_type_and_actions() {
        let scope = Scope::parse("patient/Observation.read").unwrap();
        assert_eq!(scope.subject, Subject::Patient);
        assert_eq!(scope.target, Some(kind("Observation")));
        assert_eq!(scope.actions, vec![DataAction::Read]);
        assert!(scope.permits(DataAction::Read, Some(kind("Observation"))));
        assert!(!scope.permits(DataAction::Read, Some(kind("Patient"))));
        assert!(!scope.permits(DataAction::Write, Some(kind("Observation"))));
    }

    #[test]
    fn the_short_spellings_map_to_read_and_write() {
        let scope = Scope::parse("user/*.cruds").unwrap();
        assert!(scope.permits(DataAction::Read, Some(kind("Patient"))));
        assert!(scope.permits(DataAction::Write, Some(kind("Patient"))));
        let reading = Scope::parse("user/*.rs").unwrap();
        assert!(reading.permits(DataAction::Read, Some(kind("Patient"))));
        assert!(!reading.permits(DataAction::Write, Some(kind("Patient"))));
        let writing = Scope::parse("system/Patient.cud").unwrap();
        assert!(writing.permits(DataAction::Write, Some(kind("Patient"))));
        assert!(!writing.permits(DataAction::Read, Some(kind("Patient"))));
    }

    #[test]
    fn a_wildcard_action_grants_reading_and_writing_only() {
        let scope = Scope::parse("system/*.*").unwrap();
        assert!(scope.permits(DataAction::Read, Some(kind("Patient"))));
        assert!(scope.permits(DataAction::Write, None));
        assert!(!scope.permits(DataAction::Export, None));
        assert!(!scope.permits(DataAction::ParameterManagement, None));
    }

    #[test]
    fn an_operation_action_is_granted_by_name() {
        for (raw, action) in [
            ("system/*.export", DataAction::Export),
            ("system/*.import", DataAction::Import),
            ("system/*.reindex", DataAction::Reindex),
            ("system/*.bulk-delete", DataAction::BulkDelete),
            ("system/*.bulk-update", DataAction::BulkUpdate),
            (
                "system/*.parameter-management",
                DataAction::ParameterManagement,
            ),
        ] {
            let scope = Scope::parse(raw).unwrap();
            assert!(scope.permits(action, None), "{raw}");
            assert!(!scope.permits(DataAction::Read, None), "{raw}");
            assert_eq!(action.as_str(), raw.trim_start_matches("system/*."));
        }
    }

    #[test]
    fn a_scope_may_carry_a_search_parameter_grant() {
        let scope =
            Scope::parse("patient/Observation.rs?category=laboratory&status=final").unwrap();
        assert_eq!(
            scope.filters,
            vec![
                ("category".to_owned(), "laboratory".to_owned()),
                ("status".to_owned(), "final".to_owned())
            ]
        );
        assert!(scope.permits(DataAction::Read, Some(kind("Observation"))));
    }

    #[test]
    fn a_wildcard_target_covers_every_type() {
        let scope = Scope::parse("system/*.read").unwrap();
        assert_eq!(scope.target, None);
        assert!(scope.permits(DataAction::Read, Some(kind("Encounter"))));
        assert!(scope.permits(DataAction::Read, None));
    }

    #[test]
    fn an_unreadable_scope_is_refused_and_never_silently_granted() {
        assert!(Scope::parse("openid").is_err());
        assert!(Scope::parse("system/Nonesuch.read").is_err());
        assert!(Scope::parse("other/*.read").is_err());
        assert!(Scope::parse("system/*.fly").is_err());
        assert!(Scope::parse("system/*").is_err());
    }

    #[test]
    fn a_list_keeps_the_scopes_it_can_read_and_drops_the_rest() {
        let listed = vec![
            "openid".to_owned(),
            "fhirUser".to_owned(),
            "system/Patient.read".to_owned(),
        ];
        let kept = Scope::parse_all(&listed);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target, Some(kind("Patient")));
    }
}
