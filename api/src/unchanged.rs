





















use fhir_core::Error;
use serde_json::Value;


const OWNED: &[&str] = &["versionId", "lastUpdated", "source"];



const OPTIONAL: &[&str] = &["tag", "security", "profile"];



#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unchanged {
    on: bool,
    ignored: Vec<String>,
}

impl Unchanged {
    
    
    pub fn silent() -> Unchanged {
        Unchanged::default()
    }

    
    
    
    pub fn parse(raw: &str) -> Result<Unchanged, Error> {
        let mut ignored = Vec::new();
        for name in raw
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            if !OPTIONAL.contains(&name) {
                return Err(Error::Config(format!(
                    "{name:?} is not a meta element that may be ignored; those are {}",
                    OPTIONAL.join(", ")
                )));
            }
            ignored.push(name.to_owned());
        }
        Ok(Unchanged { on: true, ignored })
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    pub fn ignoring(&self) -> &[String] {
        &self.ignored
    }

    
    pub fn holds(&self, offered: &Value, stored: &Value) -> bool {
        self.on && self.stripped(offered) == self.stripped(stored)
    }

    
    
    
    fn stripped(&self, body: &Value) -> Value {
        let mut held = body.clone();
        let Some(object) = held.as_object_mut() else {
            return held;
        };
        let Some(meta) = object.get_mut("meta").and_then(Value::as_object_mut) else {
            return held;
        };
        for name in OWNED {
            meta.remove(*name);
        }
        for name in &self.ignored {
            meta.remove(name);
        }
        let empty = meta.is_empty();
        if empty {
            object.remove("meta");
        }
        held
    }
}




pub fn outcome() -> fhir_core::OperationOutcome {
    fhir_core::OperationOutcome::information(
        "no changes were performed: what was sent is what is stored, so no version was written",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patient(extra: Value) -> Value {
        let mut held = json!({"resourceType": "Patient", "id": "p1", "active": true});
        if let (Some(object), Some(more)) = (held.as_object_mut(), extra.as_object()) {
            for (name, value) in more {
                object.insert(name.clone(), value.clone());
            }
        }
        held
    }

    #[test]
    fn an_instance_that_was_not_asked_compares_nothing_here() {
        let held = Unchanged::silent();
        assert!(!held.is_on());
        assert!(
            !held.holds(&patient(json!({})), &patient(json!({}))),
            "the store's own comparison stands; this one is not consulted"
        );
    }

    #[test]
    fn the_elements_the_server_owns_do_not_count() {
        let held = Unchanged::parse("").expect("a valid setting");
        let stored = patient(json!({
            "meta": {"versionId": "7", "lastUpdated": "2026-01-01T00:00:00Z", "source": "#abc"}
        }));
        assert!(
            held.holds(&patient(json!({})), &stored),
            "a client that sends no meta at all sent the same resource"
        );
    }

    #[test]
    fn a_label_counts_unless_an_operator_says_otherwise() {
        let stored = patient(json!({
            "meta": {"versionId": "1", "tag": [{"system": "urn:t", "code": "one"}]}
        }));
        let offered = patient(json!({}));
        assert!(
            !Unchanged::parse("")
                .expect("a setting")
                .holds(&offered, &stored),
            "a resource that lost its tag changed"
        );
        assert!(
            Unchanged::parse("tag")
                .expect("a setting")
                .holds(&offered, &stored),
            "unless the operator said tags do not count"
        );
    }

    #[test]
    fn a_changed_body_is_a_change() {
        let held = Unchanged::parse("").expect("a setting");
        let stored = patient(json!({"meta": {"versionId": "1"}}));
        let offered = json!({"resourceType": "Patient", "id": "p1", "active": false});
        assert!(!held.holds(&offered, &stored));
    }

    #[test]
    fn a_name_that_is_not_ignorable_fails_fast() {
        let error = Unchanged::parse("id").expect_err("only meta elements may be ignored");
        assert!(error.to_string().contains("tag"), "{error}");
    }
}
