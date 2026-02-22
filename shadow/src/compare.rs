use crate::case::{Answer, Case, Plan};
use crate::gate::{Class, Gate, RunFacts, Severity, Tally, Verdict};
use crate::runner::Run;
use serde_json::{Map, Value};
use std::fmt::Write as _;


#[derive(Clone, Debug)]
pub struct RequestFacts {
    
    pub method: String,
    
    pub path: String,
    
    pub headers: Vec<(String, String)>,
    
    pub body: String,
}


#[derive(Clone, Debug)]
pub struct Difference {
    
    pub case: String,
    
    pub shape: String,
    
    pub class: Class,
    
    pub severity: Severity,
    
    pub detail: String,
    
    pub request: RequestFacts,
    
    pub excused: Option<String>,
}


pub struct Report {
    
    pub gate_fingerprint: String,
    
    pub gate_terms: String,
    
    pub left: String,
    
    pub right: String,
    
    pub facts: RunFacts,
    
    pub differences: Vec<Difference>,
    
    pub verdict: Verdict,
}

fn without_authority(text: &str, base: &str) -> String {
    let mut path = text.to_owned();
    for scheme in ["http://", "https://"] {
        if let Some(rest) = text.strip_prefix(scheme) {
            path = match rest.find('/') {
                Some(slash) => rest[slash..].to_owned(),
                None => "/".to_owned(),
            };
        }
    }
    if base.is_empty() || !path.starts_with('/') {
        return path;
    }
    match path.strip_prefix(base) {
        Some("") => "/".to_owned(),
        Some(under) if under.starts_with('/') => under.to_owned(),
        _ => path,
    }
}

fn strip(value: &Value, base: &str) -> Value {
    match value {
        Value::Object(fields) => {
            let mut kept = Map::new();
            for (name, held) in fields {
                if name == "lastUpdated" || name == "versionId" {
                    continue;
                }
                kept.insert(name.clone(), strip(held, base));
            }
            Value::Object(kept)
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| strip(item, base)).collect()),
        Value::String(text) => Value::String(without_authority(text, base)),
        other => other.clone(),
    }
}

fn versions(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            if let Some(Value::String(held)) = fields.get("versionId") {
                into.push(held.clone());
            }
            for (_, held) in fields {
                versions(held, into);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| versions(item, into)),
        _ => {}
    }
}

fn kind(value: &Value) -> &str {
    value["resourceType"].as_str().unwrap_or_default()
}

fn issues(value: &Value) -> Vec<String> {
    value["issue"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|issue| {
                    format!(
                        "{}/{}",
                        issue["severity"].as_str().unwrap_or_default(),
                        issue["code"].as_str().unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn relations(value: &Value) -> Vec<String> {
    let mut found: Vec<String> = value["link"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|link| link["relation"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

fn entries(value: &Value) -> Vec<Value> {
    value["entry"].as_array().cloned().unwrap_or_default()
}

fn entry_key(entry: &Value, base: &str) -> String {
    let full = entry["fullUrl"].as_str().unwrap_or_default();
    let named = if full.is_empty() {
        format!(
            "{}/{}",
            kind(&entry["resource"]),
            entry["resource"]["id"].as_str().unwrap_or_default()
        )
    } else {
        without_authority(full, base)
    };
    format!(
        "{named}#{}@{}",
        entry["resource"]["meta"]["versionId"]
            .as_str()
            .unwrap_or_default(),
        entry["search"]["mode"].as_str().unwrap_or_default()
    )
}

fn entry_outcome(entry: &Value) -> String {
    format!(
        "{}|{}",
        entry["response"]["status"].as_str().unwrap_or_default(),
        issues(&entry["response"]["outcome"]).join(",")
    )
}

const IDENTITY: [&str; 8] = [
    "software",
    "implementation",
    "date",
    "publisher",
    "id",
    "url",
    "version",
    "name",
];

fn split_identity(value: &Value) -> (Value, Value) {
    let mut identity = Map::new();
    let mut rest = value.clone();
    if let Value::Object(fields) = &mut rest {
        for name in IDENTITY {
            if let Some(held) = fields.remove(name) {
                identity.insert(name.to_owned(), held);
            }
        }
    }
    (Value::Object(identity), rest)
}

struct Found {
    class: Class,
    detail: String,
}

fn note(into: &mut Vec<Found>, class: Class, detail: String) {
    if into.iter().any(|found| found.class == class) {
        return;
    }
    into.push(Found { class, detail });
}

fn head_of(value: &Value) -> String {
    let text = value.to_string();
    text.chars().take(240).collect()
}

fn compare_bundles(left: &Value, right: &Value, bases: (&str, &str), found: &mut Vec<Found>) {
    let searchlike = matches!(
        left["type"].as_str().unwrap_or_default(),
        "searchset" | "history"
    );
    let left_entries = entries(left);
    let right_entries = entries(right);
    if searchlike {
        let ours: Vec<String> = left_entries
            .iter()
            .map(|entry| entry_key(entry, bases.0))
            .collect();
        let theirs: Vec<String> = right_entries
            .iter()
            .map(|entry| entry_key(entry, bases.1))
            .collect();
        let mut ours_sorted = ours.clone();
        let mut theirs_sorted = theirs.clone();
        ours_sorted.sort();
        theirs_sorted.sort();
        if ours_sorted != theirs_sorted {
            note(
                found,
                Class::SearchSet,
                format!("matched {ours_sorted:?} against {theirs_sorted:?}"),
            );
        } else if ours != theirs {
            note(
                found,
                Class::SearchOrder,
                format!("ordered {ours:?} against {theirs:?}"),
            );
        }
        if left["total"] != right["total"] {
            note(
                found,
                Class::SearchSet,
                format!("counted {} against {}", left["total"], right["total"]),
            );
        }
    } else {
        let ours: Vec<String> = left_entries.iter().map(entry_outcome).collect();
        let theirs: Vec<String> = right_entries.iter().map(entry_outcome).collect();
        if ours != theirs {
            note(
                found,
                Class::BundleEntry,
                format!("answered {ours:?} against {theirs:?}"),
            );
        }
    }
    if relations(left) != relations(right) {
        note(
            found,
            Class::PagingLink,
            format!(
                "offered {:?} against {:?}",
                relations(left),
                relations(right)
            ),
        );
    }
    for ours in &left_entries {
        let key = entry_key(ours, bases.0);
        let Some(theirs) = right_entries
            .iter()
            .find(|entry| entry_key(entry, bases.1) == key)
        else {
            continue;
        };
        let our_held = strip(&ours["resource"], bases.0);
        let their_held = strip(&theirs["resource"], bases.1);
        if our_held != their_held {
            note(
                found,
                Class::Content,
                format!(
                    "entry {key} held {} against {}",
                    head_of(&our_held),
                    head_of(&their_held)
                ),
            );
        }
    }
}

fn compare_bodies(left: &Answer, right: &Answer, bases: (&str, &str), found: &mut Vec<Found>) {
    let ours: Option<Value> = serde_json::from_str(&left.body).ok();
    let theirs: Option<Value> = serde_json::from_str(&right.body).ok();
    let (ours, theirs) = match (ours, theirs) {
        (Some(ours), Some(theirs)) => (ours, theirs),
        (None, None) => {
            if left.body != right.body {
                note(
                    found,
                    Class::Content,
                    "neither answer is readable and they differ".to_owned(),
                );
            }
            return;
        }
        _ => {
            note(
                found,
                Class::Content,
                "one answer is readable and the other is not".to_owned(),
            );
            return;
        }
    };
    if ours == theirs {
        if left.body != right.body {
            note(
                found,
                Class::Representation,
                "the same content is written differently".to_owned(),
            );
        }
        return;
    }
    let mut our_versions = Vec::new();
    let mut their_versions = Vec::new();
    versions(&ours, &mut our_versions);
    versions(&theirs, &mut their_versions);
    if our_versions != their_versions {
        note(
            found,
            Class::Concurrency,
            format!("versioned {our_versions:?} against {their_versions:?}"),
        );
    }
    if kind(&ours) != kind(&theirs) {
        note(
            found,
            Class::Content,
            format!("answered a {} against a {}", kind(&ours), kind(&theirs)),
        );
        return;
    }
    if kind(&ours) == "OperationOutcome" {
        if issues(&ours) != issues(&theirs) {
            note(
                found,
                Class::OutcomeIssue,
                format!("raised {:?} against {:?}", issues(&ours), issues(&theirs)),
            );
        }
        return;
    }
    if kind(&ours) == "Bundle" {
        if ours["type"] != theirs["type"] {
            note(
                found,
                Class::Content,
                format!("bundled {} against {}", ours["type"], theirs["type"]),
            );
            return;
        }
        compare_bundles(&ours, &theirs, bases, found);
    }
    let (our_identity, our_rest) = split_identity(&ours);
    let (their_identity, their_rest) = split_identity(&theirs);
    if strip(&our_identity, bases.0) != strip(&their_identity, bases.1) {
        note(
            found,
            Class::ServerIdentity,
            format!(
                "named itself {} against {}",
                head_of(&our_identity),
                head_of(&their_identity)
            ),
        );
    }
    let mut our_rest = strip(&our_rest, bases.0);
    let mut their_rest = strip(&their_rest, bases.1);
    if kind(&ours) == "Bundle" {
        for held in [&mut our_rest, &mut their_rest] {
            if let Value::Object(fields) = held {
                fields.remove("link");
                fields.remove("entry");
                fields.remove("total");
            }
        }
    }
    if our_rest != their_rest {
        note(
            found,
            Class::Content,
            format!(
                "held {} against {}",
                head_of(&our_rest),
                head_of(&their_rest)
            ),
        );
    }
}

fn compare_headers(left: &Answer, right: &Answer, bases: (&str, &str), found: &mut Vec<Found>) {
    for name in ["etag", "last-modified"] {
        let ours = left.header(name);
        let theirs = right.header(name);
        if ours.is_some() != theirs.is_some() {
            note(
                found,
                Class::Concurrency,
                format!("{name} was offered by one side only"),
            );
        }
    }
    if let (Some(ours), Some(theirs)) = (left.header("etag"), right.header("etag")) {
        if ours != theirs {
            note(
                found,
                Class::Concurrency,
                format!("tagged {ours} against {theirs}"),
            );
        }
    }
    for name in ["location", "content-location"] {
        let ours = left
            .header(name)
            .map(|value| without_authority(value, bases.0));
        let theirs = right
            .header(name)
            .map(|value| without_authority(value, bases.1));
        if ours != theirs {
            note(
                found,
                Class::Content,
                format!("{name} named {ours:?} against {theirs:?}"),
            );
        }
    }
}

fn compare_case(left: &Answer, right: &Answer, bases: (&str, &str)) -> Vec<Found> {
    let mut found = Vec::new();
    if left.status != right.status {
        note(
            &mut found,
            Class::Status,
            format!("answered {} against {}", left.status, right.status),
        );
    }
    compare_headers(left, right, bases, &mut found);
    compare_bodies(left, right, bases, &mut found);
    found
}

fn facts_of(case: &Case) -> RequestFacts {
    RequestFacts {
        method: case.method.to_owned(),
        path: case.path.clone(),
        headers: case.headers.clone(),
        body: case.body.clone(),
    }
}


pub fn compare(plan: &Plan, run: &Run, gate: &Gate) -> Report {
    let mut differences = Vec::new();
    let mut compared = 0usize;
    let mut covered: Vec<String> = Vec::new();
    let bases = (run.left.base.as_str(), run.right.base.as_str());
    for (index, case) in plan.cases().iter().enumerate() {
        let ours = run.left.answers.get(index);
        let theirs = run.right.answers.get(index);
        match (ours, theirs) {
            (Some(Ok(ours)), Some(Ok(theirs))) => {
                compared += 1;
                if !covered.iter().any(|shape| shape == case.shape) {
                    covered.push(case.shape.to_owned());
                }
                for found in compare_case(ours, theirs, bases) {
                    let excused = gate
                        .excusing_of(found.class, &case.name, case.shape, &found.detail)
                        .map(|excused| {
                            format!("{}, rests on {}", excused.because, excused.rests_on)
                        });
                    differences.push(Difference {
                        case: case.name.clone(),
                        shape: case.shape.to_owned(),
                        class: found.class,
                        severity: gate.rule(found.class).severity,
                        detail: found.detail,
                        request: facts_of(case),
                        excused,
                    });
                }
            }
            _ => {
                let why = [ours, theirs]
                    .iter()
                    .flatten()
                    .filter_map(|answer| answer.as_ref().err())
                    .cloned()
                    .collect::<Vec<String>>()
                    .join("; ");
                differences.push(Difference {
                    case: case.name.clone(),
                    shape: case.shape.to_owned(),
                    class: Class::Unreached,
                    severity: gate.rule(Class::Unreached).severity,
                    detail: if why.is_empty() {
                        "one side answered nothing".to_owned()
                    } else {
                        why
                    },
                    request: facts_of(case),
                    excused: None,
                });
            }
        }
    }
    let tallies = Class::ALL
        .iter()
        .map(|class| Tally {
            class: *class,
            count: differences
                .iter()
                .filter(|difference| difference.class == *class && difference.excused.is_none())
                .count(),
        })
        .filter(|tally| tally.count > 0)
        .collect();
    let facts = RunFacts {
        cases_planned: plan.cases().len(),
        cases_compared: compared,
        covered,
        tallies,
    };
    let verdict = gate.judge(&facts);
    Report {
        gate_fingerprint: gate.fingerprint(),
        gate_terms: gate.render(),
        left: run.left.label.clone(),
        right: run.right.label.clone(),
        facts,
        differences,
        verdict,
    }
}

impl Report {
    
    pub fn render(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "= Difference report");
        let _ = writeln!(text);
        let _ = writeln!(text, "candidate: {}", self.left);
        let _ = writeln!(text, "incumbent: {}", self.right);
        let _ = writeln!(text, "gate: {}", self.gate_fingerprint);
        let _ = writeln!(
            text,
            "shapes: {} planned, {} compared",
            self.facts.cases_planned, self.facts.cases_compared
        );
        let _ = writeln!(text);
        let _ = writeln!(text, "== Counts");
        for class in Class::ALL {
            let held = |excused: bool| {
                self.differences
                    .iter()
                    .filter(|difference| {
                        difference.class == class && difference.excused.is_some() == excused
                    })
                    .count()
            };
            let _ = writeln!(
                text,
                "{}: {} counted, {} excused",
                class.name(),
                held(false),
                held(true)
            );
        }
        let _ = writeln!(text);
        let _ = writeln!(text, "== Verdict");
        match &self.verdict {
            Verdict::Passed => {
                let _ = writeln!(text, "passed");
            }
            Verdict::Failed(breaches) => {
                let _ = writeln!(text, "failed");
                for breach in breaches {
                    let _ = writeln!(text, "- {}", breach.reason);
                }
            }
        }
        let _ = writeln!(text);
        let _ = writeln!(text, "== Excused");
        for difference in self
            .differences
            .iter()
            .filter(|difference| difference.excused.is_some())
        {
            let _ = writeln!(
                text,
                "- {} [{}] {}: {}",
                difference.case,
                difference.shape,
                difference.class.name(),
                difference.detail
            );
            let _ = writeln!(
                text,
                "  {}",
                difference.excused.as_deref().unwrap_or_default()
            );
        }
        let _ = writeln!(text);
        let _ = writeln!(text, "== Divergences");
        for difference in self
            .differences
            .iter()
            .filter(|difference| difference.excused.is_none())
        {
            let _ = writeln!(
                text,
                "- {} [{}] {} ({})",
                difference.case,
                difference.shape,
                difference.class.name(),
                difference.severity.name()
            );
            let _ = writeln!(
                text,
                "  {} {}",
                difference.request.method, difference.request.path
            );
            for (name, value) in &difference.request.headers {
                let _ = writeln!(text, "  {name}: {value}");
            }
            if !difference.request.body.is_empty() {
                let _ = writeln!(
                    text,
                    "  body: {}",
                    difference
                        .request
                        .body
                        .chars()
                        .take(400)
                        .collect::<String>()
                );
            }
            let _ = writeln!(text, "  {}", difference.detail);
        }
        let _ = writeln!(text);
        let _ = writeln!(text, "== Gate");
        let _ = writeln!(text, "{}", self.gate_terms);
        text
    }
}
