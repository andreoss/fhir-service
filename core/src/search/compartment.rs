use crate::{ResourceId, ResourceType};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    pub resource_type: &'static str,
    pub params: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompartmentDef {
    pub code: &'static str,
    pub members: &'static [Membership],
}

impl CompartmentDef {
    pub fn member(&self, resource_type: ResourceType) -> Option<&'static Membership> {
        self.members
            .iter()
            .find(|member| member.resource_type == resource_type.as_str())
    }

    pub fn types(&self) -> Vec<&'static str> {
        self.members.iter().map(|member| member.resource_type).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compartment {
    pub kind: ResourceType,
    pub id: ResourceId,
}

const PATIENT: &[Membership] = &[
    Membership {
        resource_type: "Patient",
        params: &[],
    },
    Membership {
        resource_type: "Observation",
        params: &["subject", "patient"],
    },
    Membership {
        resource_type: "Encounter",
        params: &["subject", "patient"],
    },
    Membership {
        resource_type: "List",
        params: &["subject", "patient"],
    },
    Membership {
        resource_type: "RiskAssessment",
        params: &["subject", "patient"],
    },
];

const ENCOUNTER: &[Membership] = &[
    Membership {
        resource_type: "Encounter",
        params: &[],
    },
    Membership {
        resource_type: "Observation",
        params: &["encounter"],
    },
];

const PRACTITIONER: &[Membership] = &[
    Membership {
        resource_type: "Practitioner",
        params: &[],
    },
    Membership {
        resource_type: "Patient",
        params: &["general-practitioner"],
    },
];

const ORGANIZATION: &[Membership] = &[
    Membership {
        resource_type: "Organization",
        params: &[],
    },
    Membership {
        resource_type: "Patient",
        params: &["organization", "general-practitioner"],
    },
    Membership {
        resource_type: "Encounter",
        params: &["service-provider"],
    },
];

const DEFS: &[CompartmentDef] = &[
    CompartmentDef {
        code: "Patient",
        members: PATIENT,
    },
    CompartmentDef {
        code: "Encounter",
        members: ENCOUNTER,
    },
    CompartmentDef {
        code: "Practitioner",
        members: PRACTITIONER,
    },
    CompartmentDef {
        code: "Organization",
        members: ORGANIZATION,
    },
];

pub fn definition(code: &str) -> Option<&'static CompartmentDef> {
    DEFS.iter().find(|def| def.code == code)
}

pub fn contains(compartment: &Compartment, resource_type: ResourceType, body: &Value) -> bool {
    let Some(def) = definition(compartment.kind.as_str()) else {
        return false;
    };
    let Some(member) = def.member(resource_type) else {
        return false;
    };
    if member.params.is_empty() {
        return resource_type == compartment.kind
            && body.get("id").and_then(Value::as_str) == Some(compartment.id.as_str());
    }
    let root = format!("{}/{}", compartment.kind.as_str(), compartment.id.as_str());
    member.params.iter().any(|name| {
        crate::search::lookup(Some(resource_type), name)
            .map(|def| pointed(&def.target, body))
            .unwrap_or_default()
            .iter()
            .any(|found| found == &root || found == compartment.id.as_str())
    })
}

fn pointed(target: &crate::search::Target, body: &Value) -> Vec<String> {
    let crate::search::Target::Path(paths) = target else {
        return Vec::new();
    };
    paths
        .iter()
        .flat_map(|path| crate::search::select(body, path))
        .flat_map(crate::search::pointers)
        .map(|text| trimmed(&text))
        .collect()
}

fn trimmed(text: &str) -> String {
    let mut parts = text.rsplit('/');
    let id = parts.next().unwrap_or_default();
    match parts.next() {
        Some(kind) => format!("{kind}/{id}"),
        None => id.to_owned(),
    }
}

pub fn definitions() -> &'static [CompartmentDef] {
    DEFS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_definition_resolves_by_its_code() {
        for def in definitions() {
            assert_eq!(definition(def.code).map(|found| found.code), Some(def.code));
        }
        assert!(definition("Nonesuch").is_none());
    }

    #[test]
    fn a_root_type_belongs_to_its_own_compartment_without_a_reference() {
        let patient = definition("Patient").unwrap();
        let root = patient.member("Patient".parse().unwrap()).unwrap();
        assert!(root.params.is_empty());
        let observation = patient.member("Observation".parse().unwrap()).unwrap();
        assert_eq!(observation.params, &["subject", "patient"]);
        assert!(patient.member("Organization".parse().unwrap()).is_none());
    }

    #[test]
    fn membership_follows_the_root_identity_and_the_named_references() {
        let compartment = Compartment {
            kind: "Patient".parse().unwrap(),
            id: ResourceId::parse("p1").unwrap(),
        };
        let root = serde_json::json!({"resourceType": "Patient", "id": "p1"});
        let other = serde_json::json!({"resourceType": "Patient", "id": "p2"});
        let member = serde_json::json!({
            "resourceType": "Observation",
            "id": "o1",
            "subject": {"reference": "Patient/p1"}
        });
        let stranger = serde_json::json!({
            "resourceType": "Observation",
            "id": "o2",
            "subject": {"reference": "Patient/p2"}
        });

        assert!(contains(&compartment, "Patient".parse().unwrap(), &root));
        assert!(!contains(&compartment, "Patient".parse().unwrap(), &other));
        assert!(contains(&compartment, "Observation".parse().unwrap(), &member));
        assert!(!contains(&compartment, "Observation".parse().unwrap(), &stranger));
        assert!(!contains(&compartment, "Organization".parse().unwrap(), &root));
    }

    #[test]
    fn a_definition_lists_every_type_it_gathers() {
        let types = definition("Encounter").unwrap().types();
        assert_eq!(types, vec!["Encounter", "Observation"]);
    }

    #[test]
    fn every_named_parameter_is_registered_for_its_type() {
        for def in definitions() {
            for member in def.members {
                let resource_type: ResourceType = member.resource_type.parse().unwrap();
                for name in member.params {
                    assert!(
                        crate::search::lookup(Some(resource_type), name).is_some(),
                        "{} {name}",
                        member.resource_type
                    );
                }
            }
        }
    }
}
