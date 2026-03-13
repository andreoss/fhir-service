use crate::error::Error;
use crate::fhir_version::FhirVersion;
use crate::model::Model;
use serde_json::{Map, Number, Value};

const NAMESPACE: &str = "http://hl7.org/fhir";

const INDENT: &str = "  ";

#[derive(Clone, Copy)]
struct Form {
    pretty: bool,
    depth: usize,
}

impl Form {
    const COMPACT: Form = Form {
        pretty: false,
        depth: 0,
    };
    const PRETTY: Form = Form {
        pretty: true,
        depth: 0,
    };

    fn inner(self) -> Form {
        Form {
            depth: self.depth + 1,
            ..self
        }
    }

    fn line(self, out: &mut String) {
        if !self.pretty {
            return;
        }
        out.push('\n');
        for _ in 0..self.depth {
            out.push_str(INDENT);
        }
    }

    fn closed(self, out: &mut String, children: usize) {
        if self.pretty && children > 0 {
            self.line(out);
        }
    }
}

pub fn to_xml(version: FhirVersion, body: &Value) -> Result<String, Error> {
    written_document(version, body, Form::COMPACT)
}

pub fn to_xml_pretty(version: FhirVersion, body: &Value) -> Result<String, Error> {
    written_document(version, body, Form::PRETTY)
}

fn written_document(version: FhirVersion, body: &Value, form: Form) -> Result<String, Error> {
    let model = Model::of(version);
    let object = body
        .as_object()
        .ok_or_else(|| Error::InvalidXml("a resource is written as an object".to_owned()))?;
    let name = object
        .get("resourceType")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidXml("resourceType is absent".to_owned()))?;
    if !model.has_resource(name) {
        return Err(Error::InvalidResourceType(name.to_owned()));
    }
    let mut out = String::new();
    out.push('<');
    out.push_str(name);
    attribute(&mut out, "xmlns", NAMESPACE);
    out.push('>');
    let children = written(&mut out, form.inner(), model, name, object)?;
    form.closed(&mut out, children);
    out.push_str("</");
    out.push_str(name);
    out.push('>');
    Ok(out)
}

pub fn from_xml(version: FhirVersion, text: &str) -> Result<Value, Error> {
    let model = Model::of(version);
    let mut parser = Parser { text, at: 0 };
    let root = parser.element()?;
    parser.only_root()?;
    if !model.has_resource(&root.name) {
        return Err(Error::InvalidResourceType(root.name));
    }
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), Value::String(root.name.clone()));
    collected(&mut body, model, &root.name, &root.children)?;
    Ok(Value::Object(body))
}

pub fn tree(text: &str) -> Result<Value, Error> {
    let mut parser = Parser { text, at: 0 };
    let root = parser.element()?;
    parser.only_root()?;
    let mut body = Map::new();
    body.insert(root.name.clone(), branch(&root));
    Ok(Value::Object(body))
}

fn branch(element: &Element) -> Value {
    if element.children.is_empty() && element.attributes.is_empty() {
        return Value::String(element.text.clone());
    }
    let mut held = Map::new();
    for (name, value) in &element.attributes {
        held.insert(name.clone(), Value::String(value.clone()));
    }
    if !element.text.is_empty() {
        held.insert("value".to_owned(), Value::String(element.text.clone()));
    }
    for child in &element.children {
        let rendered = branch(child);
        match held.remove(&child.name) {
            None => {
                held.insert(child.name.clone(), rendered);
            }
            Some(Value::Array(mut items)) => {
                items.push(rendered);
                held.insert(child.name.clone(), Value::Array(items));
            }
            Some(first) => {
                held.insert(child.name.clone(), Value::Array(vec![first, rendered]));
            }
        }
    }
    Value::Object(held)
}

fn written(
    out: &mut String,
    form: Form,
    model: &Model,
    node: &str,
    object: &Map<String, Value>,
) -> Result<usize, Error> {
    let mut keys: Vec<&String> = object
        .keys()
        .filter(|key| key.as_str() != "resourceType" && !key.starts_with('_'))
        .collect();
    keys.sort_by_key(|key| model.order(node, key).unwrap_or(usize::MAX));
    let mut children = 0;
    for key in keys {
        let value = &object[key.as_str()];
        children += element(out, form, model, node, key, value, object)?;
    }
    Ok(children)
}

fn element(
    out: &mut String,
    form: Form,
    model: &Model,
    node: &str,
    key: &str,
    value: &Value,
    parent: &Map<String, Value>,
) -> Result<usize, Error> {
    if key == "div" {
        form.line(out);
        out.push_str(value.as_str().unwrap_or_default());
        return Ok(1);
    }
    let field = model.field(node, key);
    let items = match value {
        Value::Array(items) => items.as_slice(),
        other => std::slice::from_ref(other),
    };
    for (index, item) in items.iter().enumerate() {
        form.line(out);
        let shadow = shadow(parent, key, index);
        match item {
            Value::Object(_) => complex(out, form, model, node, key, item, field.as_ref())?,
            _ => primitive(out, form, model, node, key, item, shadow)?,
        }
    }
    Ok(items.len())
}

fn primitive(
    out: &mut String,
    form: Form,
    model: &Model,
    node: &str,
    key: &str,
    item: &Value,
    shadow: Option<&Value>,
) -> Result<(), Error> {
    let text = match item {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => String::new(),
        Value::Object(_) | Value::Array(_) => {
            return Err(Error::InvalidXml(format!(
                "{key} of {node} holds no single value"
            )))
        }
    };
    out.push('<');
    out.push_str(key);
    if let Some(shadow) = shadow {
        if let Some(id) = shadow.get("id").and_then(Value::as_str) {
            attribute(out, "id", id);
        }
    }
    attribute(out, "value", &text);
    let children = shadow
        .map(|shadow| {
            ["extension", "modifierExtension"]
                .iter()
                .filter_map(|key| shadow.get(*key))
                .cloned()
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default();
    if children.is_empty() {
        out.push_str("/>");
        return Ok(());
    }
    out.push('>');
    let mut held = 0;
    for child in children {
        let items = match child {
            Value::Array(items) => items,
            other => vec![other],
        };
        for item in items {
            form.inner().line(out);
            complex(
                out,
                form.inner(),
                model,
                "Extension",
                "extension",
                &item,
                model.field("Extension", "extension").as_ref(),
            )?;
            held += 1;
        }
    }
    form.closed(out, held);
    out.push_str("</");
    out.push_str(key);
    out.push('>');
    Ok(())
}

fn complex(
    out: &mut String,
    form: Form,
    model: &Model,
    node: &str,
    key: &str,
    item: &Value,
    field: Option<&crate::model::Field>,
) -> Result<(), Error> {
    let object = item
        .as_object()
        .ok_or_else(|| Error::InvalidXml(format!("{key} of {node} is written as an object")))?;
    out.push('<');
    out.push_str(key);
    let url = (key == "extension" || key == "modifierExtension")
        .then(|| object.get("url").and_then(Value::as_str))
        .flatten();
    if let Some(url) = url {
        attribute(out, "url", url);
    }
    out.push('>');
    if let Some(name) = wrapped(key, item, field) {
        form.inner().line(out);
        out.push('<');
        out.push_str(name);
        out.push('>');
        let children = written(out, form.inner().inner(), model, name, object)?;
        form.inner().closed(out, children);
        out.push_str("</");
        out.push_str(name);
        out.push('>');
        form.closed(out, 1);
        out.push_str("</");
        out.push_str(key);
        out.push('>');
        return Ok(());
    }
    let inner = child_node(model, node, key, item, field);
    let children = match url.is_some() {
        true => {
            let mut kept = object.clone();
            kept.remove("url");
            written(out, form.inner(), model, &inner, &kept)?
        }
        false => written(out, form.inner(), model, &inner, object)?,
    };
    form.closed(out, children);
    out.push_str("</");
    out.push_str(key);
    out.push('>');
    Ok(())
}

fn wrapped<'a>(key: &str, item: &'a Value, field: Option<&crate::model::Field>) -> Option<&'a str> {
    let named = item.get("resourceType").and_then(Value::as_str)?;
    let holds = key == "contained"
        || field
            .map(|field| field.type_name() == "Resource" || field.type_name() == "DomainResource")
            .unwrap_or(false);
    holds.then_some(named)
}

fn child_node(
    model: &Model,
    node: &str,
    key: &str,
    item: &Value,
    field: Option<&crate::model::Field>,
) -> String {
    if key == "contained" {
        if let Some(name) = item.get("resourceType").and_then(Value::as_str) {
            return name.to_owned();
        }
    }
    let Some(field) = field else {
        return String::new();
    };
    let code = field.type_name();
    if code == "Resource" || code == "DomainResource" {
        return item
            .get("resourceType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
    }
    let backbone = format!("{node}.{key}");
    if model.has_node(&backbone) {
        return backbone;
    }
    if model.has_node(code) {
        return code.to_owned();
    }
    String::new()
}

fn shadow<'a>(parent: &'a Map<String, Value>, key: &str, index: usize) -> Option<&'a Value> {
    let named = format!("_{key}");
    match parent.get(&named)? {
        Value::Array(items) => items.get(index),
        other => Some(other),
    }
}

fn attribute(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    escaped(out, value);
    out.push('"');
}

fn escaped(out: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            '\t' => out.push_str("&#x9;"),
            other => out.push(other),
        }
    }
}

fn collected(
    into: &mut Map<String, Value>,
    model: &Model,
    node: &str,
    children: &[Element],
) -> Result<(), Error> {
    for child in children {
        for (key, value) in element_json(model, node, child)? {
            let repeating = model
                .field(node, &key)
                .is_some_and(|field| field.repeating());
            held(into, &key, value, repeating);
        }
    }
    Ok(())
}

fn held(into: &mut Map<String, Value>, key: &str, value: Value, repeating: bool) {
    match into.get_mut(key) {
        Some(Value::Array(items)) => match value {
            Value::Array(mut more) => items.append(&mut more),
            other => items.push(other),
        },
        Some(place) => {
            let first = place.take();
            *place = Value::Array(vec![first, value]);
        }
        None => {
            let held = match repeating && !matches!(value, Value::Array(_)) {
                true => Value::Array(vec![value]),
                false => value,
            };
            into.insert(key.to_owned(), held);
        }
    }
}

fn element_json(model: &Model, node: &str, child: &Element) -> Result<Vec<(String, Value)>, Error> {
    if child.name == "div" {
        return Ok(vec![(
            "div".to_owned(),
            Value::String(child.source.clone()),
        )]);
    }
    let field = model.field(node, &child.name);
    let shape = field
        .as_ref()
        .and_then(|field| field.shape())
        .map(str::to_owned);
    if let Some(text) = child.value() {
        let mut pairs = vec![(
            child.name.clone(),
            scalar(&child.name, shape.as_deref(), &text)?,
        )];
        let mut shadow = Map::new();
        if let Some(id) = child.attribute("id") {
            shadow.insert("id".to_owned(), Value::String(id));
        }
        let mut extensions = Vec::new();
        for inner in &child.children {
            if inner.name == "extension" || inner.name == "modifierExtension" {
                extensions.push((inner.name.clone(), object_json(model, "Extension", inner)?));
            }
        }
        if !shadow.is_empty() || !extensions.is_empty() {
            let mut map = shadow;
            for (name, value) in extensions {
                held(&mut map, &name, value, true);
            }
            pairs.push((format!("_{}", child.name), Value::Object(map)));
        }
        return Ok(pairs);
    }
    if child.name == "contained"
        || field.as_ref().is_some_and(|field| {
            field.type_name() == "Resource" || field.type_name() == "DomainResource"
        })
    {
        let mut items = Vec::new();
        for inner in &child.children {
            let mut resource = Map::new();
            resource.insert("resourceType".to_owned(), Value::String(inner.name.clone()));
            collected(&mut resource, model, &inner.name, &inner.children)?;
            items.push(Value::Object(resource));
        }
        let repeating = field.as_ref().is_some_and(|field| field.repeating()) || items.len() > 1;
        let value = match repeating {
            true => Value::Array(items),
            false => items.pop().unwrap_or(Value::Object(Map::new())),
        };
        return Ok(vec![(child.name.clone(), value)]);
    }
    let node = child_node_of(model, node, child, field.as_ref());
    Ok(vec![(
        child.name.clone(),
        object_json(model, &node, child)?,
    )])
}

fn object_json(model: &Model, node: &str, child: &Element) -> Result<Value, Error> {
    let mut map = Map::new();
    if let Some(url) = child.attribute("url") {
        map.insert("url".to_owned(), Value::String(url));
    }
    collected(&mut map, model, node, &child.children)?;
    Ok(Value::Object(map))
}

fn child_node_of(
    model: &Model,
    node: &str,
    child: &Element,
    field: Option<&crate::model::Field>,
) -> String {
    if child.name == "contained" {
        if let Some(inner) = child.children.first() {
            return inner.name.clone();
        }
    }
    let Some(field) = field else {
        return String::new();
    };
    let code = field.type_name();
    if code == "Resource" || code == "DomainResource" {
        return child
            .children
            .first()
            .map(|inner| inner.name.clone())
            .unwrap_or_default();
    }
    let backbone = format!("{node}.{}", child.name);
    if model.has_node(&backbone) {
        return backbone;
    }
    if model.has_node(code) {
        return code.to_owned();
    }
    String::new()
}

fn scalar(name: &str, shape: Option<&str>, text: &str) -> Result<Value, Error> {
    match shape {
        Some("boolean") => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            other => Err(Error::InvalidXml(format!(
                "{name} is not a boolean: {other:?}"
            ))),
        },
        Some("number") => number(name, text),
        _ => Ok(Value::String(text.to_owned())),
    }
}

#[derive(Debug, Clone)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Element>,
    source: String,

    text: String,
}

impl Element {
    fn attribute(&self, name: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn value(&self) -> Option<String> {
        self.attribute("value")
    }
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn element(&mut self) -> Result<Element, Error> {
        self.seek_tag()?;
        let start = self.at;
        self.at += 1;
        let name = self.name()?;
        let mut attributes = Vec::new();
        loop {
            self.space();
            match self.peek() {
                Some('/') => {
                    self.at += 1;
                    self.expect('>')?;
                    return Ok(Element {
                        name,
                        attributes,
                        children: Vec::new(),
                        source: self.text[start..self.at].to_owned(),
                        text: String::new(),
                    });
                }
                Some('>') => {
                    self.at += 1;
                    break;
                }
                Some(_) => attributes.push(self.attribute()?),
                None => return Err(self.ended()),
            }
        }
        let mut children = Vec::new();
        let mut written = String::new();
        loop {
            written.push_str(self.characters());
            self.seek_tag()?;
            if self.text[self.at..].starts_with("</") {
                break;
            }
            children.push(self.element()?);
        }
        self.at += 2;
        let closed = self.name()?;
        self.space();
        self.expect('>')?;
        if closed != name {
            return Err(Error::InvalidXml(format!("{closed} closes {name}")));
        }
        Ok(Element {
            name,
            attributes,
            children,
            source: self.text[start..self.at].to_owned(),
            text: written.trim().to_owned(),
        })
    }

    fn characters(&mut self) -> &'a str {
        let rest = &self.text[self.at..];
        if rest.starts_with('<') {
            return "";
        }
        let Some(offset) = rest.find('<') else {
            return "";
        };
        let held = &rest[..offset];
        self.at += offset;
        held
    }

    fn only_root(&mut self) -> Result<(), Error> {
        let rest = self.text[self.at..].trim();
        if rest.is_empty() {
            Ok(())
        } else {
            Err(Error::InvalidXml(
                "one resource is written at a time".to_owned(),
            ))
        }
    }

    fn seek_tag(&mut self) -> Result<(), Error> {
        loop {
            let rest = &self.text[self.at..];
            if rest.starts_with("<!--") {
                self.past("-->")?;
            } else if rest.starts_with("<![CDATA[") {
                self.past("]]>")?;
            } else if rest.starts_with("<?") || rest.starts_with("<!") {
                self.past(">")?;
            } else if rest.starts_with('<') {
                return Ok(());
            } else if rest.is_empty() {
                return Err(self.ended());
            } else {
                let next = rest
                    .find('<')
                    .map(|offset| self.at + offset)
                    .unwrap_or(self.text.len());
                self.at = next;
            }
        }
    }

    fn past(&mut self, mark: &str) -> Result<(), Error> {
        let found = self.text[self.at + mark.len()..]
            .find(mark)
            .map(|offset| self.at + mark.len() + offset + mark.len())
            .or_else(|| {
                self.text[self.at..]
                    .find(mark)
                    .map(|offset| self.at + offset + mark.len())
            })
            .ok_or_else(|| self.ended())?;
        self.at = found;
        Ok(())
    }

    fn attribute(&mut self) -> Result<(String, String), Error> {
        let name = self.name()?;
        self.space();
        self.expect('=')?;
        self.space();
        let quote = self.peek().ok_or_else(|| self.ended())?;
        if quote != '"' && quote != '\'' {
            return Err(Error::InvalidXml("an attribute value is quoted".to_owned()));
        }
        self.at += 1;
        let from = self.at;
        let end = self.text[from..].find(quote).ok_or_else(|| self.ended())? + from;
        let value = decoded(&self.text[from..end]);
        self.at = end + 1;
        Ok((name, value))
    }

    fn name(&mut self) -> Result<String, Error> {
        let from = self.at;
        while let Some(character) = self.peek() {
            if character.is_whitespace() || character == '/' || character == '>' || character == '='
            {
                break;
            }
            self.at += 1;
        }
        if self.at == from {
            return Err(Error::InvalidXml("a name is expected".to_owned()));
        }
        Ok(self.text[from..self.at].to_owned())
    }

    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
    }

    fn expect(&mut self, character: char) -> Result<(), Error> {
        match self.peek() {
            Some(found) if found == character => {
                self.at += 1;
                Ok(())
            }
            _ => Err(Error::InvalidXml(format!("{character} is expected"))),
        }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }

    fn ended(&self) -> Error {
        Error::InvalidXml("the document ends before its element closes".to_owned())
    }
}

fn decoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(end) = after.find(';') {
            let named = &after[..end];
            if let Some(character) = entity(named) {
                out.push(character);
                rest = &after[end + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn entity(named: &str) -> Option<char> {
    match named {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        other => {
            let (digits, radix) = match other
                .strip_prefix("#x")
                .or_else(|| other.strip_prefix("#X"))
            {
                Some(digits) => (digits, 16),
                None => (other.strip_prefix('#')?, 10),
            };
            let value = u32::from_str_radix(digits, radix).ok()?;
            char::from_u32(value)
        }
    }
}

fn number(name: &str, text: &str) -> Result<Value, Error> {
    let refused = || Error::InvalidXml(format!("{name} is not a number: {text:?}"));
    if let Ok(whole) = text.parse::<i64>() {
        return Ok(Value::Number(whole.into()));
    }
    text.parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map(Value::Number)
        .ok_or_else(refused)
}
