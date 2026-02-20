use crate::{FhirVersion, ResourceId, ResourceType};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    pub resource_type: &'static str,
    pub root: bool,
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

const fn gathers(resource_type: &'static str, params: &'static [&'static str]) -> Membership {
    Membership {
        resource_type,
        root: false,
        params,
    }
}

const fn itself(resource_type: &'static str, params: &'static [&'static str]) -> Membership {
    Membership {
        resource_type,
        root: true,
        params,
    }
}

const PATIENT: &[Membership] = &[
    itself("Patient", &["link"]),
    gathers("DocumentReference", &["subject", "author"]),
    gathers("Encounter", &["patient"]),
    gathers("List", &["subject", "source"]),
    gathers("Observation", &["subject", "performer"]),
    gathers("RelatedPerson", &["patient"]),
    gathers("RiskAssessment", &["subject"]),
];

const ENCOUNTER: &[Membership] = &[
    itself("Encounter", &[]),
    gathers("DocumentReference", &["encounter", "context"]),
    gathers("Observation", &["encounter"]),
];

const RELATED_PERSON: &[Membership] = &[
    itself("RelatedPerson", &[]),
    gathers("DocumentReference", &["author"]),
    gathers("Encounter", &["participant"]),
    gathers("Observation", &["performer"]),
    gathers("Patient", &["link"]),
];

const PRACTITIONER: &[Membership] = &[
    itself("Practitioner", &[]),
    gathers("DocumentReference", &["subject", "author", "authenticator", "attester"]),
    gathers("Encounter", &["practitioner", "participant"]),
    gathers("List", &["source"]),
    gathers("Observation", &["performer"]),
    gathers("Patient", &["general-practitioner"]),
    gathers("RiskAssessment", &["performer"]),
];

const DEVICE: &[Membership] = &[
    itself("Device", &[]),
    gathers("DocumentReference", &["subject", "author"]),
    gathers("List", &["subject", "source"]),
    gathers("Observation", &["subject", "device"]),
    gathers("RiskAssessment", &["performer"]),
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
        code: "RelatedPerson",
        members: RELATED_PERSON,
    },
    CompartmentDef {
        code: "Practitioner",
        members: PRACTITIONER,
    },
    CompartmentDef {
        code: "Device",
        members: DEVICE,
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
    if member.root
        && resource_type == compartment.kind
        && body.get("id").and_then(Value::as_str) == Some(compartment.id.as_str())
    {
        return true;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedMember {
    pub resource_type: &'static str,
    pub params: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedDef {
    pub code: &'static str,
    pub members: Vec<VersionedMember>,
}

fn membership_in(version: FhirVersion, member: &Membership) -> Option<VersionedMember> {
    let resource_type: ResourceType = member.resource_type.parse().ok()?;
    let params: Vec<&'static str> = member
        .params
        .iter()
        .copied()
        .filter(|name| crate::search::lookup_in(version, Some(resource_type), name).is_some())
        .collect();
    if !member.root && params.is_empty() {
        return None;
    }
    Some(VersionedMember {
        resource_type: member.resource_type,
        params,
    })
}

pub fn definitions_in(version: FhirVersion) -> Vec<VersionedDef> {
    DEFS.iter()
        .map(|def| VersionedDef {
            code: def.code,
            members: def
                .members
                .iter()
                .filter_map(|member| membership_in(version, member))
                .collect(),
        })
        .collect()
}

pub fn definition_in(version: FhirVersion, code: &str) -> Option<VersionedDef> {
    definitions_in(version)
        .into_iter()
        .find(|def| def.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PUBLISHED: &[(&str, &str, &[&str])] = &[
        ("Patient", "Patient", &["link"]),
        ("Patient", "DocumentReference", &["subject", "author"]),
        ("Patient", "Encounter", &["patient"]),
        ("Patient", "List", &["subject", "source"]),
        ("Patient", "Observation", &["subject", "performer"]),
        ("Patient", "RelatedPerson", &["patient"]),
        ("Patient", "RiskAssessment", &["subject"]),
        ("Encounter", "Encounter", &[]),
        ("Encounter", "DocumentReference", &["encounter", "context"]),
        ("Encounter", "Observation", &["encounter"]),
        ("RelatedPerson", "RelatedPerson", &[]),
        ("RelatedPerson", "DocumentReference", &["author"]),
        ("RelatedPerson", "Encounter", &["participant"]),
        ("RelatedPerson", "Observation", &["performer"]),
        ("RelatedPerson", "Patient", &["link"]),
        ("Practitioner", "Practitioner", &[]),
        (
            "Practitioner",
            "DocumentReference",
            &["subject", "author", "authenticator", "attester"],
        ),
        ("Practitioner", "Encounter", &["practitioner", "participant"]),
        ("Practitioner", "List", &["source"]),
        ("Practitioner", "Observation", &["performer"]),
        ("Practitioner", "Patient", &["general-practitioner"]),
        ("Practitioner", "RiskAssessment", &["performer"]),
        ("Device", "Device", &[]),
        ("Device", "DocumentReference", &["subject", "author"]),
        ("Device", "List", &["subject", "source"]),
        ("Device", "Observation", &["subject", "device"]),
        ("Device", "RiskAssessment", &["performer"]),
    ];

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("the type is published")
    }

    fn compartment(code: &str, id: &str) -> Compartment {
        Compartment {
            kind: kind(code),
            id: ResourceId::parse(id).expect("the id is well formed"),
        }
    }

    #[test]
    fn the_enforced_codes_are_the_ones_compartment_type_publishes() {
        let published = ["Patient", "Encounter", "RelatedPerson", "Practitioner", "Device"];
        let mut held: Vec<&str> = definitions().iter().map(|def| def.code).collect();
        held.sort_unstable();
        let mut wanted = published.to_vec();
        wanted.sort_unstable();
        assert_eq!(held, wanted);
        for code in published {
            assert!(definition(code).is_some(), "{code}");
        }
        for absent in ["Organization", "Group", "Location", "Nonesuch"] {
            assert!(definition(absent).is_none(), "{absent}");
        }
    }

    #[test]
    fn every_membership_names_the_references_the_published_definition_names() {
        for (code, resource_type, params) in PUBLISHED {
            let def = definition(code).expect("the compartment is published");
            let member = def
                .member(kind(resource_type))
                .unwrap_or_else(|| panic!("{code} gathers {resource_type}"));
            assert_eq!(member.params, *params, "{code}/{resource_type}");
            assert_eq!(member.root, code == resource_type, "{code}/{resource_type}");
        }
        for def in definitions() {
            for member in def.members {
                assert!(
                    PUBLISHED
                        .iter()
                        .any(|(code, name, _)| *code == def.code && *name == member.resource_type),
                    "{}/{} is gathered by no published definition",
                    def.code,
                    member.resource_type
                );
            }
        }
    }

    #[test]
    fn a_patient_compartment_holds_every_reference_the_definition_names() {
        let held = compartment("Patient", "p1");
        let cases: &[(&str, Value)] = &[
            ("Patient", json!({"resourceType": "Patient", "id": "p1"})),
            (
                "Patient",
                json!({"resourceType": "Patient", "id": "p2", "link": [{"other": {"reference": "Patient/p1"}}]}),
            ),
            (
                "Observation",
                json!({"resourceType": "Observation", "subject": {"reference": "Patient/p1"}}),
            ),
            (
                "Observation",
                json!({"resourceType": "Observation", "performer": [{"reference": "Patient/p1"}]}),
            ),
            (
                "List",
                json!({"resourceType": "List", "source": {"reference": "Patient/p1"}}),
            ),
            (
                "Encounter",
                json!({"resourceType": "Encounter", "subject": {"reference": "Patient/p1"}}),
            ),
            (
                "RiskAssessment",
                json!({"resourceType": "RiskAssessment", "subject": {"reference": "Patient/p1"}}),
            ),
            (
                "RelatedPerson",
                json!({"resourceType": "RelatedPerson", "patient": {"reference": "Patient/p1"}}),
            ),
            (
                "DocumentReference",
                json!({"resourceType": "DocumentReference", "author": [{"reference": "Patient/p1"}]}),
            ),
        ];
        for (resource_type, body) in cases {
            assert!(contains(&held, kind(resource_type), body), "{resource_type} {body}");
        }
        let stranger = json!({"resourceType": "Observation", "performer": [{"reference": "Patient/p2"}]});
        assert!(!contains(&held, kind("Observation"), &stranger));
        let unrelated = json!({"resourceType": "Observation", "device": {"reference": "Patient/p1"}});
        assert!(!contains(&held, kind("Observation"), &unrelated));
        assert!(!contains(&held, kind("Organization"), &json!({"id": "p1"})));
    }

    #[test]
    fn the_device_related_person_and_practitioner_compartments_are_enforced() {
        let device = compartment("Device", "d1");
        assert!(contains(
            &device,
            kind("Observation"),
            &json!({"resourceType": "Observation", "device": {"reference": "Device/d1"}})
        ));
        assert!(contains(
            &device,
            kind("Observation"),
            &json!({"resourceType": "Observation", "subject": {"reference": "Device/d1"}})
        ));
        assert!(contains(
            &device,
            kind("Device"),
            &json!({"resourceType": "Device", "id": "d1"})
        ));
        assert!(!contains(
            &device,
            kind("Observation"),
            &json!({"resourceType": "Observation", "performer": [{"reference": "Device/d1"}]})
        ));

        let related = compartment("RelatedPerson", "rp1");
        assert!(contains(
            &related,
            kind("Encounter"),
            &json!({"resourceType": "Encounter", "participant": [{"individual": {"reference": "RelatedPerson/rp1"}}]})
        ));
        assert!(contains(
            &related,
            kind("Observation"),
            &json!({"resourceType": "Observation", "performer": [{"reference": "RelatedPerson/rp1"}]})
        ));

        let practitioner = compartment("Practitioner", "pr1");
        assert!(contains(
            &practitioner,
            kind("Patient"),
            &json!({"resourceType": "Patient", "generalPractitioner": [{"reference": "Practitioner/pr1"}]})
        ));
        assert!(contains(
            &practitioner,
            kind("RiskAssessment"),
            &json!({"resourceType": "RiskAssessment", "performer": {"reference": "Practitioner/pr1"}})
        ));
        assert!(contains(
            &practitioner,
            kind("List"),
            &json!({"resourceType": "List", "source": {"reference": "Practitioner/pr1"}})
        ));
    }

    #[test]
    fn every_version_publishes_the_references_its_own_definition_names() {
        let published: &[(FhirVersion, &str, &str, &[&str])] = &[
            (FhirVersion::Stu3, "Encounter", "Observation", &["encounter"]),
            (FhirVersion::R4, "Encounter", "Observation", &["encounter"]),
            (FhirVersion::R4b, "Encounter", "Observation", &["encounter"]),
            (FhirVersion::R5, "Encounter", "Observation", &["encounter"]),
            (FhirVersion::Stu3, "Encounter", "DocumentReference", &["encounter"]),
            (FhirVersion::R4b, "Encounter", "DocumentReference", &["encounter"]),
            (FhirVersion::R5, "Encounter", "DocumentReference", &["context"]),
            (
                FhirVersion::Stu3,
                "Practitioner",
                "DocumentReference",
                &["subject", "author", "authenticator"],
            ),
            (
                FhirVersion::R5,
                "Practitioner",
                "DocumentReference",
                &["subject", "author", "attester"],
            ),
            (FhirVersion::Stu3, "Patient", "Observation", &["subject", "performer"]),
            (FhirVersion::R5, "Patient", "Observation", &["subject", "performer"]),
            (FhirVersion::R4, "Device", "Observation", &["subject", "device"]),
        ];
        for (version, code, resource_type, params) in published {
            let def = definition_in(*version, code).expect("the compartment is published");
            let member = def
                .members
                .iter()
                .find(|member| member.resource_type == *resource_type)
                .unwrap_or_else(|| panic!("{version:?} {code}/{resource_type}"));
            assert_eq!(member.params, *params, "{version:?} {code}/{resource_type}");
        }
        for version in FhirVersion::ALL {
            assert_eq!(definitions_in(version).len(), DEFS.len(), "{version:?}");
        }
    }

    #[test]
    fn every_named_reference_reads_the_element_the_published_expression_names() {
        let published: &[(FhirVersion, &str, &str, &[&str])] = &[
            (FhirVersion::Stu3, "Observation", "encounter", &["context"]),
            (FhirVersion::Stu3, "Observation", "context", &["context"]),
            (FhirVersion::R4, "Observation", "encounter", &["encounter"]),
            (FhirVersion::R5, "Observation", "encounter", &["encounter"]),
            (FhirVersion::R4, "Observation", "performer", &["performer"]),
            (FhirVersion::R4, "Observation", "device", &["device"]),
            (FhirVersion::R4, "Patient", "link", &["link.other"]),
            (FhirVersion::R4, "List", "source", &["source"]),
            (FhirVersion::R4, "RiskAssessment", "performer", &["performer"]),
            (FhirVersion::R4, "RelatedPerson", "patient", &["patient"]),
            (
                FhirVersion::R4,
                "Encounter",
                "participant",
                &["participant.individual"],
            ),
            (FhirVersion::R5, "Encounter", "participant", &["participant.actor"]),
            (
                FhirVersion::R4,
                "DocumentReference",
                "encounter",
                &["context.encounter"],
            ),
            (FhirVersion::R5, "DocumentReference", "attester", &["attester.party"]),
        ];
        for (version, resource_type, name, paths) in published {
            let def = crate::search::lookup_in(*version, Some(kind(resource_type)), name)
                .unwrap_or_else(|| panic!("{version:?} {resource_type}.{name}"));
            assert_eq!(def.paths(), *paths, "{version:?} {resource_type}.{name}");
        }
        assert!(crate::search::lookup_in(FhirVersion::R4, Some(kind("Observation")), "context")
            .is_none());
        assert!(
            crate::search::lookup_in(FhirVersion::R5, Some(kind("DocumentReference")), "authenticator")
                .is_none()
        );
    }
}
