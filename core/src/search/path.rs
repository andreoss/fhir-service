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
    }

    #[test]
    fn arrays_are_traversed_transparently() {
        let value = json!({"name": [{"family": "Ann"}, {"family": "Bo"}]});
        let found = select(&value, "name.family");
        assert_eq!(found, vec![&json!("Ann"), &json!("Bo")]);
    }

    #[test]
    fn a_choice_segment_matches_every_named_variant() {
        let value = json!({"valueQuantity": {"value": 5}, "valueString": "x", "code": "c"});
        assert_eq!(select(&value, "value[x]").len(), 2);
        assert!(select(&value, "missing").is_empty());
        assert!(select(&json!("scalar"), "any").is_empty());
    }
}
