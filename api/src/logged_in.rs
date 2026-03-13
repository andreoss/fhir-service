



















use fhir_core::security::scope::DataAction;
use fhir_core::security::Access;
use fhir_core::{ResourceEnvelope, ResourceKey};


const SCOPE: &str = "fhirUser";


const CLAIM: &str = "fhirUser";


pub fn reading_itself(access: &Access, action: DataAction, envelope: &ResourceEnvelope) -> bool {
    if action != DataAction::Read || !access.secured {
        return false;
    }
    if !granted(access) {
        return false;
    }
    let Some(named) = access.claims.get(CLAIM) else {
        return false;
    };
    names(named, &ResourceKey::of(envelope))
}




fn granted(access: &Access) -> bool {
    access
        .claims
        .get("scope")
        .map(|held| held.split_whitespace().any(|scope| scope == SCOPE))
        .unwrap_or(false)
}




fn names(claim: &str, key: &ResourceKey) -> bool {
    let trimmed = claim.trim().trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let (Some(id), Some(kind)) = (parts.next(), parts.next()) else {
        return false;
    };
    kind == key.resource_type().as_str() && id == key.id().as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fhir_core::FhirVersion;
    use std::collections::BTreeMap;

    fn envelope(kind: &str, id: &str) -> ResourceEnvelope {
        let body = format!(
            r#"{{"resourceType":"{kind}","id":"{id}","meta":{{"versionId":"1","lastUpdated":"2026-09-06T04:00:00Z"}}}}"#
        );
        ResourceEnvelope::parse(FhirVersion::R4, body.as_bytes()).expect("a resource")
    }

    fn access(scope: &str, user: Option<&str>) -> Access {
        let mut claims = BTreeMap::new();
        claims.insert("scope".to_owned(), scope.to_owned());
        if let Some(user) = user {
            claims.insert(CLAIM.to_owned(), user.to_owned());
        }
        Access {
            actor: "someone".to_owned(),
            client: None,
            scopes: Vec::new(),
            roles: Vec::new(),
            patient: None,
            secured: true,
            claims,
        }
    }

    #[test]
    fn the_holder_may_read_the_record_its_token_names() {
        let held = access("openid fhirUser patient/*.rs", Some("Practitioner/pr1"));
        assert!(reading_itself(
            &held,
            DataAction::Read,
            &envelope("Practitioner", "pr1")
        ));
    }

    #[test]
    fn an_absolute_claim_names_the_same_record() {
        let held = access(
            "openid fhirUser",
            Some("https://ehr.example.org/fhir/Practitioner/pr1"),
        );
        assert!(reading_itself(
            &held,
            DataAction::Read,
            &envelope("Practitioner", "pr1")
        ));
    }

    #[test]
    fn it_is_one_record_and_not_a_type() {
        let held = access("openid fhirUser", Some("Practitioner/pr1"));
        assert!(!reading_itself(
            &held,
            DataAction::Read,
            &envelope("Practitioner", "pr2")
        ));
        assert!(!reading_itself(
            &held,
            DataAction::Read,
            &envelope("Patient", "pr1")
        ));
    }

    #[test]
    fn it_is_a_read_and_not_a_write() {
        let held = access("openid fhirUser", Some("Practitioner/pr1"));
        assert!(!reading_itself(
            &held,
            DataAction::Write,
            &envelope("Practitioner", "pr1")
        ));
    }

    #[test]
    fn a_token_without_the_scope_gets_nothing() {
        let held = access("openid patient/*.rs", Some("Practitioner/pr1"));
        assert!(!reading_itself(
            &held,
            DataAction::Read,
            &envelope("Practitioner", "pr1")
        ));
    }

    #[test]
    fn a_token_naming_no_user_gets_nothing() {
        let held = access("openid fhirUser", None);
        assert!(!reading_itself(
            &held,
            DataAction::Read,
            &envelope("Practitioner", "pr1")
        ));
    }

    #[test]
    fn an_unsecured_instance_is_not_affected() {
        let mut held = access("openid fhirUser", Some("Practitioner/pr1"));
        held.secured = false;
        assert!(!reading_itself(
            &held,
            DataAction::Read,
            &envelope("Practitioner", "pr1")
        ));
    }
}
