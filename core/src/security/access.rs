use crate::security::bearer::Claims;
use crate::security::scope::{DataAction, Scope};
use crate::{Error, ResourceId, ResourceType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub actor: String,
    pub client: Option<String>,
    pub scopes: Vec<Scope>,
    pub roles: Vec<String>,
    pub patient: Option<ResourceId>,
    pub secured: bool,

    pub claims: std::collections::BTreeMap<String, String>,
}

impl Access {
    pub fn open() -> Access {
        Access {
            actor: "anonymous".to_owned(),
            client: None,
            scopes: Vec::new(),
            roles: Vec::new(),
            patient: None,
            secured: false,
            claims: std::collections::BTreeMap::new(),
        }
    }

    pub fn of(claims: &Claims) -> Access {
        Access {
            actor: claims
                .subject
                .clone()
                .or_else(|| claims.client.clone())
                .unwrap_or_else(|| "unknown".to_owned()),
            client: claims.client.clone(),
            scopes: Scope::parse_all(&claims.scopes),
            roles: claims.roles.clone(),
            patient: claims
                .patient
                .as_deref()
                .and_then(|id| ResourceId::parse(id).ok()),
            secured: true,
            claims: claims.carried.clone(),
        }
    }

    pub fn permits(&self, action: DataAction, resource_type: Option<ResourceType>) -> bool {
        !self.secured
            || self
                .scopes
                .iter()
                .any(|scope| scope.permits(action, resource_type))
    }

    pub fn require(
        &self,
        action: DataAction,
        resource_type: Option<ResourceType>,
    ) -> Result<(), Error> {
        match self.permits(action, resource_type) {
            true => Ok(()),
            false => Err(Error::Forbidden(match resource_type {
                Some(kind) => format!("{} on {}", action.as_str(), kind.as_str()),
                None => action.as_str().to_owned(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a known type")
    }

    fn claims(scopes: &[&str]) -> Claims {
        Claims {
            issuer: "https://issuer.example.org".to_owned(),
            subject: Some("practitioner-1".to_owned()),
            client: Some("app-1".to_owned()),
            scopes: scopes.iter().map(|entry| (*entry).to_owned()).collect(),
            ..Claims::default()
        }
    }

    #[test]
    fn an_unsecured_instance_permits_every_action() {
        let open = Access::open();
        assert!(!open.secured);
        for action in DataAction::ALL {
            assert!(open.permits(action, Some(kind("Patient"))));
            assert!(open.require(action, None).is_ok());
        }
    }

    #[test]
    fn a_token_permits_only_what_its_scopes_carry() {
        let access = Access::of(&claims(&["system/Patient.read", "system/*.export"]));
        assert!(access.secured);
        assert_eq!(access.actor, "practitioner-1");
        assert!(access.permits(DataAction::Read, Some(kind("Patient"))));
        assert!(!access.permits(DataAction::Read, Some(kind("Observation"))));
        assert!(!access.permits(DataAction::Write, Some(kind("Patient"))));
        assert!(access.permits(DataAction::Export, None));
    }

    #[test]
    fn a_refusal_names_the_action_and_the_type_and_no_data() {
        let access = Access::of(&claims(&["system/Patient.read"]));
        let error = access
            .require(DataAction::Write, Some(kind("Patient")))
            .expect_err("writing is outside the access");
        assert!(matches!(error, Error::Forbidden(_)));
        let message = error.to_string();
        assert!(message.contains("write"));
        assert!(message.contains("Patient"));
        assert!(!message.contains("practitioner-1"));
    }

    #[test]
    fn a_token_without_a_subject_is_recorded_against_its_client() {
        let mut carried = claims(&["system/Patient.read"]);
        carried.subject = None;
        let access = Access::of(&carried);
        assert_eq!(access.actor, "app-1");
        carried.client = None;
        assert_eq!(Access::of(&carried).actor, "unknown");
    }

    #[test]
    fn a_launch_compartment_travels_with_the_access() {
        let mut carried = claims(&["patient/Observation.rs"]);
        carried.patient = Some("pt-1".to_owned());
        let access = Access::of(&carried);
        assert_eq!(
            access.patient.map(|id| id.as_str().to_owned()),
            Some("pt-1".to_owned())
        );
    }

    #[test]
    fn a_token_carrying_no_usable_scope_permits_nothing() {
        let access = Access::of(&claims(&["openid", "fhirUser"]));
        for action in DataAction::ALL {
            assert!(!access.permits(action, Some(kind("Patient"))));
        }
    }
}
