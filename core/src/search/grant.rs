use crate::search::compartment::Compartment;
use crate::search::Filter;
use crate::{Error, ResourceEnvelope, ResourceId, ResourceType};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Grant {
    pub types: Vec<ResourceType>,
    pub compartments: Vec<Compartment>,
    pub filters: Vec<GrantFilter>,
    
    
    
    
    pub every: Vec<Filter>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrantFilter {
    pub resource_type: ResourceType,
    pub filter: Filter,
}

impl Grant {
    pub fn parse(raw: &str) -> Result<Grant, Error> {
        let mut grant = Grant::default();
        for entry in raw
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let (key, value) = entry
                .split_once('=')
                .ok_or_else(|| Error::InvalidParameter(format!("grant entry {entry:?}")))?;
            match key.trim() {
                "types" => grant.types.extend(parsed_types(value)?),
                "compartment" => grant.compartments.push(compartment(value)?),
                other => return Err(Error::UnsupportedParameter(format!("grant key {other:?}"))),
            }
        }
        Ok(grant)
    }

    pub fn admits(&self, resource_type: ResourceType) -> bool {
        self.types.is_empty() || self.types.contains(&resource_type)
    }

    pub fn is_open(&self) -> bool {
        self.compartments.is_empty()
    }

    pub fn reaches(&self, envelope: &ResourceEnvelope, body: &Value) -> bool {
        if !self.admits(envelope.resource_type()) {
            return false;
        }
        let narrowed = self
            .narrowing(envelope.resource_type())
            .into_iter()
            .chain(self.every.iter())
            .all(|filter| filter.matches(envelope.id(), envelope.last_updated(), body));
        narrowed
            && (self.is_open()
                || self.compartments.iter().any(|compartment| {
                    crate::search::compartment::contains(
                        compartment,
                        envelope.resource_type(),
                        body,
                    )
                }))
    }

    pub fn narrowing(&self, resource_type: ResourceType) -> Vec<&Filter> {
        self.filters
            .iter()
            .filter(|held| held.resource_type == resource_type)
            .map(|held| &held.filter)
            .collect()
    }
}

fn parsed_types(raw: &str) -> Result<Vec<ResourceType>, Error> {
    raw.split(',')
        .filter(|part| !part.is_empty())
        .map(str::parse::<ResourceType>)
        .collect()
}

fn compartment(raw: &str) -> Result<Compartment, Error> {
    let (kind, id) = raw
        .split_once('/')
        .ok_or_else(|| Error::InvalidParameter(format!("grant compartment {raw:?}")))?;
    Ok(Compartment {
        kind: kind.parse::<ResourceType>()?,
        id: ResourceId::parse(id)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{SearchValue, Target, ValueType};
    use crate::FhirVersion;
    use serde_json::json;

    fn kind(name: &str) -> ResourceType {
        name.parse().expect("the type is published")
    }

    fn held(body: &Value) -> (ResourceEnvelope, Value) {
        let bytes = serde_json::to_vec(body).expect("the body renders");
        let envelope = ResourceEnvelope::parse_supplied(FhirVersion::R4, &bytes)
            .expect("the body is a resource");
        (envelope, body.clone())
    }

    fn observation(id: &str, subject: &str) -> Value {
        json!({
            "resourceType": "Observation",
            "id": id,
            "status": "final",
            "subject": {"reference": subject}
        })
    }

    #[test]
    fn a_grant_carries_types_and_compartments() {
        let grant = Grant::parse("types=Patient,Observation;compartment=Patient/pt-1").unwrap();
        assert!(grant.admits(kind("Patient")));
        assert!(!grant.admits(kind("Encounter")));
        assert!(!grant.is_open());
        assert_eq!(grant.compartments[0].id.as_str(), "pt-1");
    }

    #[test]
    fn an_empty_grant_admits_everything() {
        let grant = Grant::parse("").unwrap();
        assert!(grant.admits(kind("Encounter")));
        assert!(grant.is_open());
        assert_eq!(grant, Grant::default());
    }

    #[test]
    fn a_malformed_grant_is_rejected() {
        assert!(matches!(
            Grant::parse("types").unwrap_err(),
            Error::InvalidParameter(_)
        ));
        assert!(matches!(
            Grant::parse("compartment=Patient").unwrap_err(),
            Error::InvalidParameter(_)
        ));
        assert!(matches!(
            Grant::parse("scope=all").unwrap_err(),
            Error::UnsupportedParameter(_)
        ));
        assert!(Grant::parse("types=Nonesuch").is_err());
        assert!(Grant::parse("compartment=Patient/not a id").is_err());
    }

    #[test]
    fn a_grant_reaches_nothing_of_a_type_it_does_not_admit() {
        let grant = Grant::parse("types=Patient").unwrap();
        let (envelope, body) = held(&observation("ob-1", "Patient/pt-1"));
        assert!(!grant.reaches(&envelope, &body));
        let (patient, patient_body) = held(&json!({"resourceType": "Patient", "id": "pt-1"}));
        assert!(grant.reaches(&patient, &patient_body));
    }

    #[test]
    fn a_grant_confined_to_a_compartment_reaches_only_its_members() {
        let grant = Grant::parse("compartment=Patient/pt-1").unwrap();
        let (inside, inside_body) = held(&observation("ob-1", "Patient/pt-1"));
        let (outside, outside_body) = held(&observation("ob-2", "Patient/pt-2"));
        assert!(grant.reaches(&inside, &inside_body));
        assert!(!grant.reaches(&outside, &outside_body));
        let (root, root_body) = held(&json!({"resourceType": "Patient", "id": "pt-1"}));
        assert!(grant.reaches(&root, &root_body));
        let (other, other_body) = held(&json!({"resourceType": "Patient", "id": "pt-2"}));
        assert!(!grant.reaches(&other, &other_body));
        let (ungathered, ungathered_body) =
            held(&json!({"resourceType": "Organization", "id": "pt-1"}));
        assert!(!grant.reaches(&ungathered, &ungathered_body));
    }

    #[test]
    fn a_condition_narrows_the_type_it_names_and_no_other() {
        let final_only = Filter::new(
            "status",
            Target::path(["status"]),
            vec![SearchValue::parse(ValueType::Token, "final").unwrap()],
        );
        let grant = Grant {
            types: Vec::new(),
            compartments: Vec::new(),
            filters: vec![GrantFilter {
                resource_type: kind("Observation"),
                filter: final_only,
            }],
            every: Vec::new(),
        };
        let (matching, matching_body) = held(&observation("ob-1", "Patient/pt-1"));
        assert!(grant.reaches(&matching, &matching_body));
        let mut draft = observation("ob-2", "Patient/pt-1");
        draft["status"] = json!("preliminary");
        let (refused, refused_body) = held(&draft);
        assert!(!grant.reaches(&refused, &refused_body));
        let (untouched, untouched_body) = held(&json!({"resourceType": "Patient", "id": "pt-1"}));
        assert!(grant.reaches(&untouched, &untouched_body));
        assert_eq!(grant.narrowing(kind("Observation")).len(), 1);
        assert!(grant.narrowing(kind("Patient")).is_empty());
    }

    #[test]
    fn every_restriction_of_a_grant_holds_at_once() {
        let mut grant = Grant::parse("types=Observation;compartment=Patient/pt-1").unwrap();
        grant.filters.push(GrantFilter {
            resource_type: kind("Observation"),
            filter: Filter::new(
                "status",
                Target::path(["status"]),
                vec![SearchValue::parse(ValueType::Token, "final").unwrap()],
            ),
        });
        let (admitted, admitted_body) = held(&observation("ob-1", "Patient/pt-1"));
        assert!(grant.reaches(&admitted, &admitted_body));
        let mut wrong_status = observation("ob-2", "Patient/pt-1");
        wrong_status["status"] = json!("preliminary");
        let (refused, refused_body) = held(&wrong_status);
        assert!(!grant.reaches(&refused, &refused_body));
        let (elsewhere, elsewhere_body) = held(&observation("ob-3", "Patient/pt-2"));
        assert!(!grant.reaches(&elsewhere, &elsewhere_body));
        let (wrong_type, wrong_type_body) = held(&json!({"resourceType": "Patient", "id": "pt-1"}));
        assert!(!grant.reaches(&wrong_type, &wrong_type_body));
    }
}
