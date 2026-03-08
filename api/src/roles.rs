use fhir_core::security::scope::DataAction;
use fhir_core::security::Access;
use fhir_core::{Error, FhirVersion, ResourceType};

const ANY: &str = "*";



#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    pub actions: Vec<DataAction>,
    pub types: Vec<ResourceType>,
}

impl Role {
    fn permits(&self, action: DataAction, resource_type: Option<ResourceType>) -> bool {
        if !self.actions.contains(&action) {
            return false;
        }
        match (self.types.is_empty(), resource_type) {
            (true, _) => true,
            (false, None) => false,
            (false, Some(kind)) => self.types.contains(&kind),
        }
    }
}



#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roles {
    named: Vec<Role>,
    fallback: Option<Role>,
}

impl Roles {
    
    
    
    
    
    pub fn parse(raw: &str, version: FhirVersion) -> Result<Roles, Error> {
        let mut held = Roles::default();
        for part in raw
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let (name, rest) = part.split_once('=').ok_or_else(|| {
                Error::Config(format!(
                    "role {part:?} names no actions; write name=actions"
                ))
            })?;
            let (actions, types) = match rest.split_once(':') {
                None => (rest, ""),
                Some((actions, types)) => (actions, types),
            };
            let role = Role {
                name: name.trim().to_owned(),
                actions: parsed_actions(actions)?,
                types: parsed_types(types, version)?,
            };
            match role.name.as_str() {
                ANY => held.fallback = Some(role),
                _ => held.named.push(role),
            }
        }
        Ok(held)
    }

    pub fn is_empty(&self) -> bool {
        self.named.is_empty() && self.fallback.is_none()
    }

    
    
    
    pub fn permits(
        &self,
        access: &Access,
        action: DataAction,
        resource_type: Option<ResourceType>,
    ) -> bool {
        if self.is_empty() || !access.secured {
            return true;
        }
        let matched: Vec<&Role> = self
            .named
            .iter()
            .filter(|role| access.roles.iter().any(|held| held == &role.name))
            .collect();
        if matched.is_empty() {
            return self
                .fallback
                .as_ref()
                .is_some_and(|role| role.permits(action, resource_type));
        }
        matched
            .iter()
            .any(|role| role.permits(action, resource_type))
    }

    
    
    
    pub fn require(
        &self,
        access: &Access,
        action: DataAction,
        resource_type: Option<ResourceType>,
    ) -> Result<(), Error> {
        match self.permits(access, action, resource_type) {
            true => Ok(()),
            false => Err(Error::Forbidden(match resource_type {
                Some(kind) => format!("{} on {}", action.as_str(), kind.as_str()),
                None => action.as_str().to_owned(),
            })),
        }
    }
}

fn parsed_actions(raw: &str) -> Result<Vec<DataAction>, Error> {
    let mut held = Vec::new();
    for name in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if name == ANY {
            held.extend(DataAction::ALL);
            continue;
        }
        let action = DataAction::ALL
            .into_iter()
            .find(|action| action.as_str() == name)
            .ok_or_else(|| {
                Error::Config(format!(
                    "role action {name:?} names none this service takes"
                ))
            })?;
        held.push(action);
    }
    if held.is_empty() {
        return Err(Error::Config("a role names at least one action".to_owned()));
    }
    held.sort_by_key(|action| action.as_str());
    held.dedup();
    Ok(held)
}

fn parsed_types(raw: &str, version: FhirVersion) -> Result<Vec<ResourceType>, Error> {
    let mut held = Vec::new();
    for name in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if name == ANY {
            return Ok(Vec::new());
        }
        let kind = name.parse::<ResourceType>().map_err(|_| {
            Error::Config(format!("role names {name:?}, which is no resource type"))
        })?;
        if !ResourceType::served(version).contains(&kind) {
            return Err(Error::Config(format!(
                "role names {kind}, which {version} does not serve"
            )));
        }
        held.push(kind);
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::security::scope::Scope;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("a served type")
    }

    fn holder(roles: &[&str]) -> Access {
        Access {
            actor: "practitioner-1".to_owned(),
            client: None,
            scopes: Scope::parse_all(&["system/*.*".to_owned()]),
            roles: roles.iter().map(|held| (*held).to_owned()).collect(),
            patient: None,
            secured: true,
            claims: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn nothing_configured_leaves_the_scopes_to_decide() {
        let held = Roles::default();
        assert!(held.is_empty());
        assert!(held.permits(&holder(&[]), DataAction::Write, Some(kind("Patient"))));
    }

    #[test]
    fn a_role_allows_the_actions_it_names_and_no_others() {
        let held = Roles::parse("clinician=read,write", FhirVersion::R4).unwrap();
        let who = holder(&["clinician"]);
        assert!(held.permits(&who, DataAction::Read, Some(kind("Patient"))));
        assert!(held.permits(&who, DataAction::Write, Some(kind("Patient"))));
        assert!(!held.permits(&who, DataAction::Export, Some(kind("Patient"))));
    }

    #[test]
    fn a_role_confined_to_types_reaches_no_others() {
        let held = Roles::parse("auditor=read:AuditEvent", FhirVersion::R4).unwrap();
        let who = holder(&["auditor"]);
        assert!(held.permits(&who, DataAction::Read, Some(kind("AuditEvent"))));
        assert!(!held.permits(&who, DataAction::Read, Some(kind("Patient"))));
        assert!(
            !held.permits(&who, DataAction::Read, None),
            "a confined role does not reach an action over every type"
        );
    }

    #[test]
    fn a_token_carrying_no_recognised_role_is_allowed_nothing_without_a_fallback() {
        let held = Roles::parse("clinician=read", FhirVersion::R4).unwrap();
        assert!(!held.permits(
            &holder(&["porter"]),
            DataAction::Read,
            Some(kind("Patient"))
        ));
    }

    #[test]
    fn a_fallback_role_catches_a_token_no_role_matched() {
        let held = Roles::parse("clinician=read,write;*=read", FhirVersion::R4).unwrap();
        let who = holder(&["porter"]);
        assert!(held.permits(&who, DataAction::Read, Some(kind("Patient"))));
        assert!(!held.permits(&who, DataAction::Write, Some(kind("Patient"))));
    }

    #[test]
    fn two_roles_are_taken_together() {
        let held = Roles::parse("reader=read;writer=write", FhirVersion::R4).unwrap();
        let who = holder(&["reader", "writer"]);
        assert!(held.permits(&who, DataAction::Read, Some(kind("Patient"))));
        assert!(held.permits(&who, DataAction::Write, Some(kind("Patient"))));
    }

    #[test]
    fn an_unsecured_instance_consults_no_role() {
        let held = Roles::parse("clinician=read", FhirVersion::R4).unwrap();
        assert!(held.permits(&Access::open(), DataAction::Write, Some(kind("Patient"))));
    }

    #[test]
    fn a_refusal_names_the_action_and_the_type_and_nothing_of_the_token() {
        let held = Roles::parse("clinician=read", FhirVersion::R4).unwrap();
        let error = held
            .require(
                &holder(&["clinician"]),
                DataAction::Write,
                Some(kind("Patient")),
            )
            .unwrap_err();
        let told = error.to_string();
        assert!(told.contains("write"), "{told}");
        assert!(told.contains("Patient"), "{told}");
        assert!(!told.contains("clinician"), "{told}");
        assert!(!told.contains("practitioner-1"), "{told}");
    }

    #[test]
    fn an_unknown_action_is_refused_at_startup() {
        let error = Roles::parse("clinician=peek", FhirVersion::R4).unwrap_err();
        assert!(error.to_string().contains("peek"), "{error}");
    }

    #[test]
    fn an_unknown_type_is_refused_at_startup() {
        let error = Roles::parse("clinician=read:Nonesuch", FhirVersion::R4).unwrap_err();
        assert!(error.to_string().contains("Nonesuch"), "{error}");
    }

    #[test]
    fn a_type_the_release_does_not_serve_is_refused_at_startup() {
        let error = Roles::parse("clinician=read:Citation", FhirVersion::Stu3).unwrap_err();
        assert!(error.to_string().contains("does not serve"), "{error}");
    }

    #[test]
    fn a_role_naming_no_action_is_refused_at_startup() {
        assert!(Roles::parse("clinician", FhirVersion::R4).is_err());
        assert!(Roles::parse("clinician=", FhirVersion::R4).is_err());
    }

    #[test]
    fn a_star_action_names_every_action() {
        let held = Roles::parse("admin=*", FhirVersion::R4).unwrap();
        let who = holder(&["admin"]);
        for action in DataAction::ALL {
            assert!(held.permits(&who, action, Some(kind("Patient"))));
        }
    }
}
