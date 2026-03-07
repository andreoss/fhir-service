use serde_json::Value;

pub fn select<'a>(value: &'a Value, path: &str) -> Vec<&'a Value> {
    let mut current = vec![value];
    for segment in path.split('.').filter(|part| !part.is_empty()) {
        let mut next = Vec::new();
        for item in current {
            collect(item, segment, &mut next);
        }
        current = next;
    }
    current
}

fn collect<'a>(value: &'a Value, segment: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect(item, segment, out);
            }
        }
        Value::Object(map) => match segment.strip_suffix("[x]") {
            Some(stem) => {
                for (key, found) in map {
                    if key.starts_with(stem) {
                        out.push(found);
                    }
                }
            }
            None => {
                if let Some(found) = map.get(segment) {
                    out.push(found);
                }
            }
        },
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_empty_path_denotes_the_value_itself() {
        let value = json!({"a": 1});
        assert_eq!(select(&value, ""), vec![&value]);
        assert_eq!(select(&value, "."), vec![&value]);
    }

    #[test]
    fn a_repeating_element_yields_one_selection_for_each_repetition() {
        let value = json!({"name": [{"family": "Ann"}, {"family": "Bo"}]});
        assert_eq!(
            select(&value, "name.family"),
            vec![&json!("Ann"), &json!("Bo")]
        );
        let nested = json!({"name": [{"given": ["Ana", "Maria"]}, {"given": ["Bo"]}]});
        assert_eq!(
            select(&nested, "name.given"),
            vec![&json!(["Ana", "Maria"]), &json!(["Bo"])]
        );
        let partial = json!({"name": [{"family": "Ann"}, {"given": ["Bo"]}]});
        assert_eq!(select(&partial, "name.family"), vec![&json!("Ann")]);
    }

    #[test]
    fn a_choice_element_is_named_by_the_stem_the_specification_spells_it_with() {
        let value = json!({"valueQuantity": {"value": 5}, "valueString": "x", "code": "c"});
        let found = select(&value, "value[x]");
        assert_eq!(found.len(), 2);
        assert!(found.contains(&&json!({"value": 5})));
        assert!(found.contains(&&json!("x")));
        assert!(!found.contains(&&json!("c")));
        assert!(select(&json!({"code": "c"}), "value[x]").is_empty());
    }

    #[test]
    fn a_path_that_does_not_resolve_selects_nothing() {
        assert!(select(&json!({"a": 1}), "missing").is_empty());
        assert!(select(&json!({"a": 1}), "a.b.c").is_empty());
        assert!(select(&json!("scalar"), "any").is_empty());
        assert!(select(&json!(null), "any").is_empty());
        assert!(select(&json!([]), "any").is_empty());
    }

    #[test]
    fn a_present_element_carrying_no_value_is_selected_as_null() {
        let value = json!({"deceasedBoolean": null});
        assert_eq!(select(&value, "deceasedBoolean"), vec![&json!(null)]);
    }
}
