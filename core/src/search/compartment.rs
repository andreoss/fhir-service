use crate::{ResourceId, ResourceType};

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
