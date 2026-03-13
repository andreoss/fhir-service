use fhir_core::security::scope::{DataAction, Scope};
use fhir_core::security::Access;
use fhir_core::{Error, ResourceType};
use fhir_store::{ResourceStore, SearchQuery};
use serde_json::Value;

pub const FHIR_USER: &str = "fhirUser";

pub const ACCESS_POLICY: &str = "AccessPolicy";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policies {
    on: bool,
}

impl Policies {
    pub fn off() -> Policies {
        Policies::default()
    }

    pub fn on() -> Result<Policies, Error> {
        fhir_core::resource_type::register(ACCESS_POLICY)?;
        Ok(Policies { on: true })
    }

    pub fn is_on(&self) -> bool {
        self.on
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub subjects: Vec<String>,
    pub scopes: Vec<Scope>,
}

impl Policy {
    pub fn parse(body: &Value) -> Policy {
        let texts = |name: &str| {
            body.get(name)
                .and_then(Value::as_array)
                .map(|held| {
                    held.iter()
                        .filter_map(|entry| match entry {
                            Value::String(text) => Some(text.clone()),
                            other => other
                                .get("reference")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        })
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default()
        };
        Policy {
            subjects: texts("subject"),
            scopes: Scope::parse_all(&texts("scope")),
        }
    }

    pub fn names(&self, user: &str) -> bool {
        self.subjects.iter().any(|held| same_user(held, user))
    }

    pub fn permits(&self, action: DataAction, resource_type: Option<ResourceType>) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope.permits(action, resource_type))
    }
}

fn same_user(one: &str, other: &str) -> bool {
    let tail = |held: &str| {
        let trimmed = held.trim_end_matches('/');
        let mut parts = trimmed.rsplit('/');
        let id = parts.next().unwrap_or_default().to_owned();
        let kind = parts.next().unwrap_or_default().to_owned();
        format!("{kind}/{id}")
    };
    !one.is_empty() && !other.is_empty() && tail(one) == tail(other)
}

pub async fn naming(store: &dyn ResourceStore, user: &str) -> Result<Vec<Policy>, Error> {
    let Ok(kind) = ACCESS_POLICY.parse::<ResourceType>() else {
        return Ok(Vec::new());
    };
    let page = store.search(&SearchQuery::of_type(kind)).await?;
    Ok(page
        .entries
        .iter()
        .filter_map(|envelope| serde_json::from_slice::<Value>(envelope.raw()).ok())
        .map(|body| Policy::parse(&body))
        .filter(|policy| policy.names(user))
        .collect())
}

pub fn user_of(access: &Access) -> Option<&str> {
    access
        .claims
        .get(FHIR_USER)
        .map(String::as_str)
        .filter(|held| !held.is_empty())
}

pub async fn require(
    policies: Policies,
    store: &dyn ResourceStore,
    access: &Access,
    action: DataAction,
    resource_type: Option<ResourceType>,
) -> Result<(), Error> {
    if !policies.is_on() || !access.secured {
        return Ok(());
    }
    let Some(user) = user_of(access) else {
        return Ok(());
    };
    let held = naming(store, user).await?;
    if held.is_empty() {
        return Ok(());
    }
    match held
        .iter()
        .any(|policy| policy.permits(action, resource_type))
    {
        true => Ok(()),
        false => Err(Error::Forbidden(match resource_type {
            Some(kind) => format!("{} on {}", action.as_str(), kind.as_str()),
            None => action.as_str().to_owned(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(subjects: Value, scopes: Value) -> Policy {
        Policy::parse(&json!({
            "resourceType": ACCESS_POLICY,
            "subject": subjects,
            "scope": scopes,
        }))
    }

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a known type")
    }

    #[test]
    fn a_policy_names_its_subjects_either_way_they_are_written() {
        let held = policy(
            json!([{"reference": "Practitioner/p1"}, "Practitioner/p2"]),
            json!(["user/Observation.read"]),
        );
        assert!(held.names("Practitioner/p1"));
        assert!(held.names("Practitioner/p2"));
        assert!(!held.names("Practitioner/p3"));
    }

    #[test]
    fn an_absolute_reference_names_the_same_user_as_a_relative_one() {
        let held = policy(json!(["Practitioner/p1"]), json!(["user/Observation.read"]));
        assert!(held.names("https://ehr.example.org/fhir/Practitioner/p1"));
        assert!(!held.names("https://ehr.example.org/fhir/Patient/p1"));
    }

    #[test]
    fn a_policy_allows_what_its_scopes_allow_and_nothing_else() {
        let held = policy(
            json!(["Practitioner/p1"]),
            json!(["user/Observation.read", "user/Condition.*"]),
        );
        assert!(held.permits(DataAction::Read, Some(kind("Observation"))));
        assert!(!held.permits(DataAction::Write, Some(kind("Observation"))));
        assert!(held.permits(DataAction::Write, Some(kind("Condition"))));
        assert!(!held.permits(DataAction::Read, Some(kind("Patient"))));
    }

    #[test]
    fn a_policy_naming_no_scope_allows_nothing() {
        let held = policy(json!(["Practitioner/p1"]), json!([]));
        assert!(!held.permits(DataAction::Read, Some(kind("Observation"))));
        assert!(
            held.names("Practitioner/p1"),
            "and it still names its subject, so it is the one in force"
        );
    }
}
