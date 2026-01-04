use crate::Error;
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Patch {
    Json(Vec<JsonOperation>),
    Path(Vec<PathOperation>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonOperation {
    Add { path: String, value: Value },
    Remove { path: String },
    Replace { path: String, value: Value },
    Move { from: String, path: String },
    Copy { from: String, path: String },
    Test { path: String, value: Value },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOperation {
    Add { path: String, name: String, value: Value },
    Insert { path: String, index: usize, value: Value },
    Delete { path: String },
    Replace { path: String, value: Value },
    Move { path: String, source: usize, destination: usize },
}

impl Patch {
    pub fn parse(bytes: &[u8]) -> Result<Patch, Error> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| Error::InvalidJson(e.to_string()))?;
        match &value {
            Value::Array(operations) => parse_json_patch(operations),
            Value::Object(object) if object.get("resourceType") == Some(&Value::String("Parameters".to_owned())) => {
                parse_path_patch(object)
            }
            _ => Err(Error::InvalidPatch(
                "expected a json patch array or a parameters resource".to_owned(),
            )),
        }
    }

    pub fn apply(&self, document: &[u8]) -> Result<Vec<u8>, Error> {
        let mut value: Value = serde_json::from_slice(document).map_err(|e| Error::InvalidJson(e.to_string()))?;
        match self {
            Patch::Json(operations) => {
                for operation in operations {
                    apply_json(&mut value, operation)?;
                }
            }
            Patch::Path(operations) => {
                for operation in operations {
                    apply_path(&mut value, operation)?;
                }
            }
        }
        serde_json::to_vec(&value).map_err(|e| Error::InvalidJson(e.to_string()))
    }
}

fn parse_json_patch(operations: &[Value]) -> Result<Patch, Error> {
    let mut parsed = Vec::with_capacity(operations.len());
    for operation in operations {
        let object = operation
            .as_object()
            .ok_or_else(|| Error::InvalidPatch("each operation must be an object".to_owned()))?;
        let op = text(object, "op")?;
        let parsed_operation = match op.as_str() {
            "add" => JsonOperation::Add { path: text(object, "path")?, value: member(object, "value")? },
            "remove" => JsonOperation::Remove { path: text(object, "path")? },
            "replace" => JsonOperation::Replace { path: text(object, "path")?, value: member(object, "value")? },
            "move" => JsonOperation::Move { from: text(object, "from")?, path: text(object, "path")? },
            "copy" => JsonOperation::Copy { from: text(object, "from")?, path: text(object, "path")? },
            "test" => JsonOperation::Test { path: text(object, "path")?, value: member(object, "value")? },
            other => return Err(Error::InvalidPatch(format!("unsupported operation {other:?}"))),
        };
        parsed.push(parsed_operation);
    }
    Ok(Patch::Json(parsed))
}

fn parse_path_patch(object: &Map<String, Value>) -> Result<Patch, Error> {
    let parameters = object
        .get("parameter")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidPatch("parameters must hold a parameter array".to_owned()))?;
    let mut parsed = Vec::new();
    for parameter in parameters {
        let entry = parameter
            .as_object()
            .ok_or_else(|| Error::InvalidPatch("each parameter must be an object".to_owned()))?;
        if entry.get("name") != Some(&Value::String("operation".to_owned())) {
            return Err(Error::InvalidPatch("every parameter must be named operation".to_owned()));
        }
        let parts = entry
            .get("part")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidPatch("an operation must hold its parts".to_owned()))?;
        parsed.push(parse_path_operation(parts)?);
    }
    Ok(Patch::Path(parsed))
}

fn parse_path_operation(parts: &[Value]) -> Result<PathOperation, Error> {
    let kind = part_text(parts, "type")?;
    let path = part_text(parts, "path")?;
    match kind.as_str() {
        "add" => Ok(PathOperation::Add {
            path,
            name: part_text(parts, "name")?,
            value: part_value(parts)?,
        }),
        "insert" => Ok(PathOperation::Insert {
            path,
            index: part_index(parts, "index")?,
            value: part_value(parts)?,
        }),
        "delete" => Ok(PathOperation::Delete { path }),
        "replace" => Ok(PathOperation::Replace { path, value: part_value(parts)? }),
        "move" => Ok(PathOperation::Move {
            path,
            source: part_index(parts, "source")?,
            destination: part_index(parts, "destination")?,
        }),
        other => Err(Error::InvalidPatch(format!("unsupported operation {other:?}"))),
    }
}

fn text(object: &Map<String, Value>, name: &str) -> Result<String, Error> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidPatch(format!("missing or non-string {name}")))
}

fn member(object: &Map<String, Value>, name: &str) -> Result<Value, Error> {
    object
        .get(name)
        .cloned()
        .ok_or_else(|| Error::InvalidPatch(format!("missing {name}")))
}

fn part<'a>(parts: &'a [Value], name: &str) -> Option<&'a Map<String, Value>> {
    parts
        .iter()
        .filter_map(Value::as_object)
        .find(|part| part.get("name") == Some(&Value::String(name.to_owned())))
}

fn part_text(parts: &[Value], name: &str) -> Result<String, Error> {
    let entry = part(parts, name).ok_or_else(|| Error::InvalidPatch(format!("missing part {name}")))?;
    entry
        .iter()
        .find(|(key, _)| key.starts_with("value"))
        .and_then(|(_, value)| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidPatch(format!("part {name} must carry a string value")))
}

fn part_index(parts: &[Value], name: &str) -> Result<usize, Error> {
    let entry = part(parts, name).ok_or_else(|| Error::InvalidPatch(format!("missing part {name}")))?;
    entry
        .iter()
        .find(|(key, _)| key.starts_with("value"))
        .and_then(|(_, value)| value.as_u64())
        .map(|index| index as usize)
        .ok_or_else(|| Error::InvalidPatch(format!("part {name} must carry a whole number")))
}

fn part_value(parts: &[Value]) -> Result<Value, Error> {
    let entry = part(parts, "value").ok_or_else(|| Error::InvalidPatch("missing part value".to_owned()))?;
    entry
        .iter()
        .find(|(key, _)| key.starts_with("value"))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| Error::InvalidPatch("part value must carry a value".to_owned()))
}

fn pointer_tokens(path: &str) -> Result<Vec<String>, Error> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let rest = path
        .strip_prefix('/')
        .ok_or_else(|| Error::InvalidPatch(format!("{path:?} is not a json pointer")))?;
    Ok(rest
        .split('/')
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
        .collect())
}

fn resolve<'a>(root: &'a Value, tokens: &[String]) -> Result<&'a Value, Error> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Value::Object(map) => map
                .get(token)
                .ok_or_else(|| Error::InvalidPatch(format!("no member {token:?}")))?,
            Value::Array(items) => {
                let index = array_index(token, items.len(), false)?;
                items
                    .get(index)
                    .ok_or_else(|| Error::InvalidPatch(format!("index {index} is out of range")))?
            }
            _ => return Err(Error::InvalidPatch(format!("cannot descend into {token:?}"))),
        };
    }
    Ok(current)
}

fn array_index(token: &str, len: usize, appending: bool) -> Result<usize, Error> {
    if appending && token == "-" {
        return Ok(len);
    }
    let index = token
        .parse::<usize>()
        .map_err(|_| Error::InvalidPatch(format!("{token:?} is not an array index")))?;
    let limit = if appending { len } else { len.saturating_sub(1) };
    if index > limit {
        return Err(Error::InvalidPatch(format!("index {index} is out of range")));
    }
    Ok(index)
}

fn split_last(tokens: &[String]) -> Result<(&[String], &String), Error> {
    tokens
        .split_last()
        .map(|(last, head)| (head, last))
        .ok_or_else(|| Error::InvalidPatch("the document root cannot be the target".to_owned()))
}

fn parent<'a>(root: &'a mut Value, tokens: &[String]) -> Result<&'a mut Value, Error> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Value::Object(map) => map
                .get_mut(token)
                .ok_or_else(|| Error::InvalidPatch(format!("no member {token:?}")))?,
            Value::Array(items) => {
                let index = array_index(token, items.len(), false)?;
                items
                    .get_mut(index)
                    .ok_or_else(|| Error::InvalidPatch(format!("index {index} is out of range")))?
            }
            _ => return Err(Error::InvalidPatch(format!("cannot descend into {token:?}"))),
        };
    }
    Ok(current)
}

fn insert_at(root: &mut Value, tokens: &[String], value: Value) -> Result<(), Error> {
    let (head, last) = split_last(tokens)?;
    match parent(root, head)? {
        Value::Object(map) => {
            map.insert(last.clone(), value);
            Ok(())
        }
        Value::Array(items) => {
            let index = array_index(last, items.len(), true)?;
            items.insert(index, value);
            Ok(())
        }
        _ => Err(Error::InvalidPatch(format!("cannot add {last:?} to a leaf"))),
    }
}

fn remove_at(root: &mut Value, tokens: &[String]) -> Result<Value, Error> {
    let (head, last) = split_last(tokens)?;
    match parent(root, head)? {
        Value::Object(map) => map
            .remove(last.as_str())
            .ok_or_else(|| Error::InvalidPatch(format!("no member {last:?}"))),
        Value::Array(items) => {
            let index = array_index(last, items.len(), false)?;
            Ok(items.remove(index))
        }
        _ => Err(Error::InvalidPatch(format!("cannot remove {last:?} from a leaf"))),
    }
}

fn replace_at(root: &mut Value, tokens: &[String], value: Value) -> Result<(), Error> {
    let (head, last) = split_last(tokens)?;
    match parent(root, head)? {
        Value::Object(map) => match map.get_mut(last.as_str()) {
            Some(slot) => {
                *slot = value;
                Ok(())
            }
            None => Err(Error::InvalidPatch(format!("no member {last:?}"))),
        },
        Value::Array(items) => {
            let index = array_index(last, items.len(), false)?;
            items[index] = value;
            Ok(())
        }
        _ => Err(Error::InvalidPatch(format!("cannot replace {last:?} in a leaf"))),
    }
}

fn apply_json(root: &mut Value, operation: &JsonOperation) -> Result<(), Error> {
    match operation {
        JsonOperation::Add { path, value } => insert_at(root, &pointer_tokens(path)?, value.clone()),
        JsonOperation::Remove { path } => remove_at(root, &pointer_tokens(path)?).map(|_| ()),
        JsonOperation::Replace { path, value } => replace_at(root, &pointer_tokens(path)?, value.clone()),
        JsonOperation::Move { from, path } => {
            let taken = remove_at(root, &pointer_tokens(from)?)?;
            insert_at(root, &pointer_tokens(path)?, taken)
        }
        JsonOperation::Copy { from, path } => {
            let taken = resolve(root, &pointer_tokens(from)?)?.clone();
            insert_at(root, &pointer_tokens(path)?, taken)
        }
        JsonOperation::Test { path, value } => {
            let found = resolve(root, &pointer_tokens(path)?)?;
            if found == value {
                Ok(())
            } else {
                Err(Error::InvalidPatch(format!("test failed at {path:?}")))
            }
        }
    }
}

fn path_tokens(path: &str) -> Result<Vec<String>, Error> {
    let mut tokens = Vec::new();
    for (position, segment) in path.split('.').enumerate() {
        if segment.is_empty() {
            return Err(Error::InvalidPatch(format!("{path:?} has an empty step")));
        }
        let (name, index) = match segment.split_once('[') {
            Some((name, rest)) => {
                let digits = rest
                    .strip_suffix(']')
                    .ok_or_else(|| Error::InvalidPatch(format!("{segment:?} has an unclosed index")))?;
                (name, Some(digits.to_owned()))
            }
            None => (segment, None),
        };
        if position > 0 || name.chars().next().is_some_and(char::is_lowercase) {
            tokens.push(name.to_owned());
        }
        if let Some(index) = index {
            tokens.push(index);
        }
    }
    Ok(tokens)
}

fn apply_path(root: &mut Value, operation: &PathOperation) -> Result<(), Error> {
    match operation {
        PathOperation::Replace { path, value } => replace_at(root, &path_tokens(path)?, value.clone()),
        PathOperation::Delete { path } => remove_at(root, &path_tokens(path)?).map(|_| ()),
        PathOperation::Add { path, name, value } => {
            let mut tokens = path_tokens(path)?;
            tokens.push(name.clone());
            match parent(root, &tokens[..tokens.len() - 1])? {
                Value::Object(map) => match map.get_mut(name.as_str()) {
                    Some(Value::Array(items)) => {
                        items.push(value.clone());
                        Ok(())
                    }
                    _ => {
                        map.insert(name.clone(), value.clone());
                        Ok(())
                    }
                },
                _ => Err(Error::InvalidPatch(format!("cannot add {name:?} to a leaf"))),
            }
        }
        PathOperation::Insert { path, index, value } => match parent(root, &path_tokens(path)?)? {
            Value::Array(items) => {
                if *index > items.len() {
                    return Err(Error::InvalidPatch(format!("index {index} is out of range")));
                }
                items.insert(*index, value.clone());
                Ok(())
            }
            _ => Err(Error::InvalidPatch(format!("{path:?} is not a list"))),
        },
        PathOperation::Move { path, source, destination } => match parent(root, &path_tokens(path)?)? {
            Value::Array(items) => {
                if *source >= items.len() || *destination > items.len().saturating_sub(1) {
                    return Err(Error::InvalidPatch("move is out of range".to_owned()));
                }
                let taken = items.remove(*source);
                items.insert(*destination, taken);
                Ok(())
            }
            _ => Err(Error::InvalidPatch(format!("{path:?} is not a list"))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATIENT: &[u8] = br#"{"resourceType":"Patient","id":"pt-1","active":true,"name":[{"family":"One"},{"family":"Two"}]}"#;

    fn apply(patch: &[u8]) -> Result<Value, Error> {
        let patched = Patch::parse(patch)?.apply(PATIENT)?;
        Ok(serde_json::from_slice(&patched).unwrap())
    }

    #[test]
    fn json_patch_replaces_a_member() {
        let value = apply(br#"[{"op":"replace","path":"/active","value":false}]"#).unwrap();
        assert_eq!(value["active"], false);
    }

    #[test]
    fn json_patch_adds_removes_copies_and_moves() {
        let value = apply(
            br#"[{"op":"add","path":"/gender","value":"female"},
                 {"op":"copy","from":"/gender","path":"/language"},
                 {"op":"move","from":"/language","path":"/text"},
                 {"op":"remove","path":"/active"},
                 {"op":"add","path":"/name/-","value":{"family":"Three"}}]"#,
        )
        .unwrap();
        assert_eq!(value["gender"], "female");
        assert_eq!(value["text"], "female");
        assert!(value.get("language").is_none());
        assert!(value.get("active").is_none());
        assert_eq!(value["name"][2]["family"], "Three");
    }

    #[test]
    fn json_patch_test_guards_the_change() {
        let ok = apply(br#"[{"op":"test","path":"/active","value":true},{"op":"replace","path":"/active","value":false}]"#);
        assert_eq!(ok.unwrap()["active"], false);
        let failed = apply(br#"[{"op":"test","path":"/active","value":false}]"#);
        assert!(matches!(failed, Err(Error::InvalidPatch(_))));
    }

    #[test]
    fn json_patch_indexes_an_array_member() {
        let value = apply(br#"[{"op":"replace","path":"/name/1/family","value":"Second"}]"#).unwrap();
        assert_eq!(value["name"][1]["family"], "Second");
        assert_eq!(value["name"][0]["family"], "One");
    }

    #[test]
    fn json_patch_rejects_an_unknown_path() {
        assert!(matches!(
            apply(br#"[{"op":"replace","path":"/gender","value":"female"}]"#),
            Err(Error::InvalidPatch(_))
        ));
        assert!(matches!(
            apply(br#"[{"op":"remove","path":"/name/9"}]"#),
            Err(Error::InvalidPatch(_))
        ));
    }

    #[test]
    fn json_patch_rejects_an_unknown_operation() {
        assert!(matches!(
            apply(br#"[{"op":"upsert","path":"/active","value":1}]"#),
            Err(Error::InvalidPatch(_))
        ));
    }

    #[test]
    fn a_failing_operation_leaves_the_document_untouched() {
        let patch = Patch::parse(
            br#"[{"op":"replace","path":"/active","value":false},{"op":"remove","path":"/gender"}]"#,
        )
        .unwrap();
        assert!(patch.apply(PATIENT).is_err());
        let original: Value = serde_json::from_slice(PATIENT).unwrap();
        assert_eq!(original["active"], true);
    }

    #[test]
    fn path_patch_replaces_a_member() {
        let value = apply(
            br#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[
                {"name":"type","valueCode":"replace"},
                {"name":"path","valueString":"Patient.active"},
                {"name":"value","valueBoolean":false}]}]}"#,
        )
        .unwrap();
        assert_eq!(value["active"], false);
    }

    #[test]
    fn path_patch_reaches_into_a_list() {
        let value = apply(
            br#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[
                {"name":"type","valueCode":"replace"},
                {"name":"path","valueString":"Patient.name[1].family"},
                {"name":"value","valueString":"Second"}]}]}"#,
        )
        .unwrap();
        assert_eq!(value["name"][1]["family"], "Second");
    }

    #[test]
    fn path_patch_adds_inserts_deletes_and_moves() {
        let value = apply(
            br#"{"resourceType":"Parameters","parameter":[
                {"name":"operation","part":[
                    {"name":"type","valueCode":"add"},
                    {"name":"path","valueString":"Patient"},
                    {"name":"name","valueString":"gender"},
                    {"name":"value","valueCode":"female"}]},
                {"name":"operation","part":[
                    {"name":"type","valueCode":"insert"},
                    {"name":"path","valueString":"Patient.name"},
                    {"name":"index","valueInteger":0},
                    {"name":"value","valueString":"Zero"}]},
                {"name":"operation","part":[
                    {"name":"type","valueCode":"move"},
                    {"name":"path","valueString":"Patient.name"},
                    {"name":"source","valueInteger":0},
                    {"name":"destination","valueInteger":1}]},
                {"name":"operation","part":[
                    {"name":"type","valueCode":"delete"},
                    {"name":"path","valueString":"Patient.active"}]}]}"#,
        )
        .unwrap();
        assert_eq!(value["gender"], "female");
        assert_eq!(value["name"][1], "Zero");
        assert!(value.get("active").is_none());
    }

    #[test]
    fn path_patch_appends_to_an_existing_list() {
        let value = apply(
            br#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[
                {"name":"type","valueCode":"add"},
                {"name":"path","valueString":"Patient"},
                {"name":"name","valueString":"name"},
                {"name":"value","valueString":"Extra"}]}]}"#,
        )
        .unwrap();
        assert_eq!(value["name"][2], "Extra");
    }

    #[test]
    fn path_patch_rejects_an_unknown_path_or_operation() {
        assert!(matches!(
            apply(
                br#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[
                    {"name":"type","valueCode":"replace"},
                    {"name":"path","valueString":"Patient.gender"},
                    {"name":"value","valueCode":"female"}]}]}"#
            ),
            Err(Error::InvalidPatch(_))
        ));
        assert!(matches!(
            apply(
                br#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[
                    {"name":"type","valueCode":"upsert"},
                    {"name":"path","valueString":"Patient.active"}]}]}"#
            ),
            Err(Error::InvalidPatch(_))
        ));
    }

    #[test]
    fn a_patch_document_of_another_shape_is_rejected() {
        assert!(matches!(Patch::parse(br#"{"resourceType":"Patient"}"#), Err(Error::InvalidPatch(_))));
        assert!(matches!(Patch::parse(b"not json"), Err(Error::InvalidJson(_))));
    }

    #[test]
    fn a_pointer_must_be_rooted() {
        assert!(matches!(
            apply(br#"[{"op":"remove","path":"active"}]"#),
            Err(Error::InvalidPatch(_))
        ));
    }
}
