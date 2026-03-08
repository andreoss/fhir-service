















use fhir_core::{terminology, Catalogue, Error, FhirVersion, Model};
use serde_json::{json, Map, Value};

const MIX: u64 = 0x9e37_79b9_7f4a_7c15;


pub const UNVERIFIED_SYSTEMS: [&str; 3] = [LOINC, SNOMED, UCUM];

pub const LOINC: &str = "http://loinc.org";
pub const SNOMED: &str = "http://snomed.info/sct";
pub const UCUM: &str = "http://unitsofmeasure.org";

const TAG: &str = "urn:generated";

fn mixed(seed: u64, position: u64) -> u64 {
    let mut value = seed.wrapping_add(position.wrapping_mul(MIX));
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}



#[derive(Clone, Copy)]
pub struct Draw {
    seed: u64,
    position: u64,
}

impl Draw {
    pub fn new(seed: u64, position: u64) -> Draw {
        Draw { seed, position }
    }

    fn value(&self, salt: u64) -> u64 {
        mixed(self.seed, self.position.wrapping_mul(97).wrapping_add(salt))
    }

    fn pick<'a, T>(&self, salt: u64, from: &'a [T]) -> Option<&'a T> {
        match from.is_empty() {
            true => None,
            false => from.get((self.value(salt) % from.len() as u64) as usize),
        }
    }

    fn between(&self, salt: u64, low: u64, high: u64) -> u64 {
        match high <= low {
            true => low,
            false => low + self.value(salt) % (high - low),
        }
    }

    fn date(&self, salt: u64, first: u64, last: u64) -> String {
        let year = self.between(salt, first, last);
        let month = 1 + self.value(salt ^ 0x11) % 12;
        let day = 1 + self.value(salt ^ 0x22) % 28;
        format!("{year:04}-{month:02}-{day:02}")
    }

    fn moment(&self, salt: u64, first: u64, last: u64) -> String {
        let hour = self.value(salt ^ 0x33) % 24;
        let minute = self.value(salt ^ 0x44) % 60;
        let second = self.value(salt ^ 0x55) % 60;
        let day = self.date(salt, first, last);
        format!("{day}T{hour:02}:{minute:02}:{second:02}Z")
    }
}




fn put(
    model: &Model,
    node: &str,
    into: &mut Map<String, Value>,
    named: &str,
    value: Value,
) -> bool {
    if model.field(node, named).is_none() {
        return false;
    }
    into.insert(named.to_owned(), value);
    true
}





fn put_any<'a>(
    model: &Model,
    node: &str,
    into: &mut Map<String, Value>,
    named: &[&'a str],
    value: Value,
) -> Option<&'a str> {
    named
        .iter()
        .find(|candidate| put(model, node, into, candidate, value.clone()))
        .copied()
}






pub fn coded(
    model: &Model,
    node: &str,
    named: &str,
    preferred: &[&str],
    draw: &Draw,
    salt: u64,
) -> Option<String> {
    let field = model.field(node, named)?;
    let codes = field.codes();
    if codes.is_empty() {
        return None;
    }
    for want in preferred {
        if codes.iter().any(|code| code == want) {
            return Some((*want).to_owned());
        }
    }
    draw.pick(salt, codes).cloned()
}

const FAMILY: [&str; 12] = [
    "Abrams",
    "Bennett",
    "Castillo",
    "Delaney",
    "Eriksen",
    "Fournier",
    "Ghosh",
    "Halvorsen",
    "Ibrahim",
    "Jankowski",
    "Kowalczyk",
    "Lindqvist",
];
const GIVEN: [&str; 12] = [
    "Aurelia",
    "Benedict",
    "Camille",
    "Dimitri",
    "Elowen",
    "Fabian",
    "Giselle",
    "Hector",
    "Imogen",
    "Joaquin",
    "Katarzyna",
    "Lucian",
];
const CITY: [&str; 6] = [
    "Ashford",
    "Brookvale",
    "Carrowden",
    "Dunmoor",
    "Elmsworth",
    "Fairhollow",
];




const OBSERVED: [(&str, &str, &str, &str, u64, u64); 5] = [
    ("29463-7", "Body weight", "kg", "kg", 45, 120),
    ("8867-4", "Heart rate", "beats/minute", "/min", 48, 104),
    (
        "8480-6",
        "Systolic blood pressure",
        "mmHg",
        "mm[Hg]",
        95,
        165,
    ),
    (
        "8462-4",
        "Diastolic blood pressure",
        "mmHg",
        "mm[Hg]",
        55,
        100,
    ),
    ("2708-6", "Oxygen saturation", "%", "%", 92, 100),
];

const PROCEDURES: [(&str, &str); 6] = [
    ("80146002", "Appendicectomy"),
    ("274031008", "Rectal examination"),
    ("35637008", "Blood transfusion"),
    ("18286008", "Wound dressing"),
    ("387731002", "Physiotherapy"),
    ("103693007", "Diagnostic procedure"),
];

fn tag(seed: u64) -> Value {
    json!({ "tag": [{ "system": TAG, "code": format!("{seed}") }] })
}

pub fn patient(model: &Model, id: &str, seed: u64, draw: &Draw) -> Value {
    let family = draw.pick(1, &FAMILY).copied().unwrap_or("Abrams");
    let given = draw.pick(2, &GIVEN).copied().unwrap_or("Aurelia");
    let city = draw.pick(3, &CITY).copied().unwrap_or("Ashford");
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), json!("Patient"));
    let node = "Patient";
    put(model, node, &mut body, "id", json!(id));
    put(model, node, &mut body, "meta", tag(seed));
    put(
        model,
        node,
        &mut body,
        "identifier",
        json!([{ "system": "urn:generated:record", "value": format!("{:016x}", draw.value(4)) }]),
    );
    put(
        model,
        node,
        &mut body,
        "active",
        json!(!draw.value(5).is_multiple_of(10)),
    );
    put(
        model,
        node,
        &mut body,
        "name",
        json!([{ "use": "official", "family": family, "given": [given] }]),
    );
    put(
        model,
        node,
        &mut body,
        "telecom",
        json!([{
            "system": "phone",
            "value": format!("+1-555-{:04}", draw.value(6) % 10_000),
            "use": "home"
        }]),
    );
    if let Some(gender) = coded(model, node, "gender", &[], draw, 7) {
        body.insert("gender".to_owned(), json!(gender));
    }
    put(
        model,
        node,
        &mut body,
        "birthDate",
        json!(draw.date(8, 1930, 2016)),
    );
    put(
        model,
        node,
        &mut body,
        "address",
        json!([{
            "use": "home",
            "line": [format!("{} {} Street", draw.between(9, 1, 400), family)],
            "city": city,
            "postalCode": format!("{:05}", draw.value(10) % 100_000),
            "country": "GB"
        }]),
    );
    Value::Object(body)
}

pub fn observation(
    model: &Model,
    id: &str,
    subject: &str,
    seed: u64,
    draw: &Draw,
    ordinal: usize,
) -> Value {
    let (code, display, unit, symbol, low, high) = OBSERVED[ordinal % OBSERVED.len()];
    let salt = 100 + ordinal as u64;
    let node = "Observation";
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), json!("Observation"));
    put(model, node, &mut body, "id", json!(id));
    put(model, node, &mut body, "meta", tag(seed));
    if let Some(status) = coded(model, node, "status", &["final"], draw, salt) {
        body.insert("status".to_owned(), json!(status));
    }
    put(
        model,
        node,
        &mut body,
        "category",
        json!([{ "coding": [{
            "system": "http://terminology.hl7.org/CodeSystem/observation-category",
            "code": "vital-signs",
            "display": "Vital Signs"
        }] }]),
    );
    put(
        model,
        node,
        &mut body,
        "code",
        json!({ "coding": [{ "system": LOINC, "code": code, "display": display }], "text": display }),
    );
    put(
        model,
        node,
        &mut body,
        "subject",
        json!({ "reference": subject }),
    );
    put(
        model,
        node,
        &mut body,
        "effectiveDateTime",
        json!(draw.moment(salt ^ 0x7, 2015, 2026)),
    );
    put(
        model,
        node,
        &mut body,
        "valueQuantity",
        json!({
            "value": draw.between(salt ^ 0x9, low, high),
            "unit": unit,
            "system": UCUM,
            "code": symbol
        }),
    );
    put(
        model,
        node,
        &mut body,
        "referenceRange",
        json!([{
            "low": { "value": low, "unit": unit, "system": UCUM, "code": symbol },
            "high": { "value": high, "unit": unit, "system": UCUM, "code": symbol }
        }]),
    );
    Value::Object(body)
}

pub fn procedure(model: &Model, id: &str, subject: &str, seed: u64, draw: &Draw) -> Value {
    let (code, display) = PROCEDURES[(draw.value(200) % PROCEDURES.len() as u64) as usize];
    let node = "Procedure";
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), json!("Procedure"));
    put(model, node, &mut body, "id", json!(id));
    put(model, node, &mut body, "meta", tag(seed));
    if let Some(status) = coded(model, node, "status", &["completed"], draw, 201) {
        body.insert("status".to_owned(), json!(status));
    }
    put(
        model,
        node,
        &mut body,
        "code",
        json!({ "coding": [{ "system": SNOMED, "code": code, "display": display }], "text": display }),
    );
    put(
        model,
        node,
        &mut body,
        "subject",
        json!({ "reference": subject }),
    );
    put_any(
        model,
        node,
        &mut body,
        &["performedDateTime", "occurrenceDateTime"],
        json!(draw.moment(202, 2015, 2026)),
    );
    Value::Object(body)
}





pub fn defines(version: FhirVersion, system: &str, code: &str) -> bool {
    let Some(body) = Catalogue::of(version).system(system, None) else {
        return false;
    };
    terminology::flattened(&terminology::concepts(body))
        .iter()
        .any(|held| held.code == code)
}






fn shaped(
    model: &Model,
    node: &str,
    named: &str,
    system: &str,
    code: &str,
    display: &str,
) -> Option<Value> {
    let field = model.field(node, named)?;
    let coding = json!({ "system": system, "code": code, "display": display });
    let one = match field.type_name() {
        "code" => json!(code),
        "Coding" => coding,
        "CodeableConcept" => json!({ "coding": [coding], "text": display }),
        "string" => json!(display),
        _ => return None,
    };
    match field.repeating() {
        true => Some(json!([one])),
        false => Some(one),
    }
}

const ACT_CODE: &str = "http://terminology.hl7.org/CodeSystem/v3-ActCode";
const CONDITION_CLINICAL: &str = "http://terminology.hl7.org/CodeSystem/condition-clinical";
const DOC_TYPE: &str = "http://loinc.org";


const CLASSES: [(&str, &str); 4] = [
    ("AMB", "ambulatory"),
    ("IMP", "inpatient encounter"),
    ("EMER", "emergency"),
    ("HH", "home health"),
];




const CONDITIONS: [(&str, &str, u64); 6] = [
    ("38341003", "Hypertension", 12),
    ("44054006", "Type 2 diabetes mellitus", 15),
    ("195967001", "Asthma", 20),
    ("13645005", "Chronic obstructive lung disease", 10),
    ("35489007", "Depressive disorder", 3),
    ("396275006", "Osteoarthritis", 8),
];

const MEDICATIONS: [(&str, &str, &str, u64); 4] = [
    ("386864001", "Amlodipine", "mg", 5),
    ("372567009", "Metformin", "mg", 500),
    ("391781002", "Salbutamol", "ug", 100),
    ("387467008", "Sertraline", "mg", 50),
];

pub fn encounter(
    model: &Model,
    id: &str,
    subject: &str,
    seed: u64,
    draw: &Draw,
    ordinal: usize,
) -> Value {
    let (code, display) =
        CLASSES[(draw.value(300 + ordinal as u64) % CLASSES.len() as u64) as usize];
    let salt = 300 + ordinal as u64;
    let node = "Encounter";
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), json!("Encounter"));
    put(model, node, &mut body, "id", json!(id));
    put(model, node, &mut body, "meta", tag(seed));
    if let Some(status) = coded(
        model,
        node,
        "status",
        &["finished", "completed"],
        draw,
        salt,
    ) {
        body.insert("status".to_owned(), json!(status));
    }
    if let Some(class) = shaped(model, node, "class", ACT_CODE, code, display) {
        body.insert("class".to_owned(), class);
    }
    put(
        model,
        node,
        &mut body,
        "subject",
        json!({ "reference": subject }),
    );
    let year = 2015 + (ordinal as u64 % 10);
    put_any(
        model,
        node,
        &mut body,
        &["period", "actualPeriod"],
        json!({
            "start": draw.moment(salt ^ 0xa1, year, year + 1),
            "end": draw.moment(salt ^ 0xa2, year, year + 1)
        }),
    );
    Value::Object(body)
}

pub fn condition(
    model: &Model,
    id: &str,
    subject: &str,
    seed: u64,
    draw: &Draw,
    ordinal: usize,
) -> Value {
    let (code, display, _years) =
        CONDITIONS[(draw.value(400 + ordinal as u64) % CONDITIONS.len() as u64) as usize];
    let salt = 400 + ordinal as u64;
    let node = "Condition";
    let resolved = draw.value(salt ^ 0xb0).is_multiple_of(3);
    let clinical = match resolved {
        true => "resolved",
        false => "active",
    };
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), json!("Condition"));
    put(model, node, &mut body, "id", json!(id));
    put(model, node, &mut body, "meta", tag(seed));
    if let Some(status) = shaped(
        model,
        node,
        "clinicalStatus",
        CONDITION_CLINICAL,
        clinical,
        clinical,
    ) {
        body.insert("clinicalStatus".to_owned(), status);
    }
    put(
        model,
        node,
        &mut body,
        "code",
        json!({ "coding": [{ "system": SNOMED, "code": code, "display": display }], "text": display }),
    );
    put(
        model,
        node,
        &mut body,
        "subject",
        json!({ "reference": subject }),
    );
    let onset = 2000 + draw.value(salt ^ 0xb1) % 20;
    put(
        model,
        node,
        &mut body,
        "onsetDateTime",
        json!(draw.moment(salt ^ 0xb2, onset, onset + 1)),
    );
    if resolved {
        put(
            model,
            node,
            &mut body,
            "abatementDateTime",
            json!(draw.moment(salt ^ 0xb3, onset + 1, onset + 4)),
        );
    }
    Value::Object(body)
}

pub fn medication(model: &Model, id: &str, subject: &str, seed: u64, draw: &Draw) -> Value {
    let (code, display, unit, dose) =
        MEDICATIONS[(draw.value(500) % MEDICATIONS.len() as u64) as usize];
    let node = "MedicationRequest";
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), json!("MedicationRequest"));
    put(model, node, &mut body, "id", json!(id));
    put(model, node, &mut body, "meta", tag(seed));
    if let Some(status) = coded(model, node, "status", &["active"], draw, 501) {
        body.insert("status".to_owned(), json!(status));
    }
    if let Some(intent) = coded(model, node, "intent", &["order"], draw, 502) {
        body.insert("intent".to_owned(), json!(intent));
    }
    let concept = json!({ "coding": [{ "system": SNOMED, "code": code, "display": display }], "text": display });
    if put(
        model,
        node,
        &mut body,
        "medicationCodeableConcept",
        concept.clone(),
    ) {
    } else {
        put(
            model,
            node,
            &mut body,
            "medication",
            json!({ "concept": concept }),
        );
    }
    put_any(
        model,
        node,
        &mut body,
        &["subject"],
        json!({ "reference": subject }),
    );
    put(
        model,
        node,
        &mut body,
        "authoredOn",
        json!(draw.moment(503, 2018, 2026)),
    );
    let quantity = json!({ "value": dose, "unit": unit, "system": UCUM, "code": unit });
    let mut dosage = json!({
        "text": format!("{dose} {unit} once daily"),
        "timing": { "repeat": { "frequency": 1, "period": 1, "periodUnit": "d" } }
    });
    let amount = match model.field("Dosage", "doseAndRate").is_some() {
        true => ("doseAndRate", json!([{ "doseQuantity": quantity }])),
        false => ("doseQuantity", quantity),
    };
    if let Some(object) = dosage.as_object_mut() {
        object.insert(amount.0.to_owned(), amount.1);
    }
    put(model, node, &mut body, "dosageInstruction", json!([dosage]));

    Value::Object(body)
}

pub fn document(model: &Model, id: &str, subject: &str, seed: u64, draw: &Draw) -> Value {
    let node = "DocumentReference";
    let mut body = Map::new();
    body.insert("resourceType".to_owned(), json!("DocumentReference"));
    put(model, node, &mut body, "id", json!(id));
    put(model, node, &mut body, "meta", tag(seed));
    if let Some(status) = coded(model, node, "status", &["current"], draw, 600) {
        body.insert("status".to_owned(), json!(status));
    }
    put(
        model,
        node,
        &mut body,
        "type",
        json!({ "coding": [{ "system": DOC_TYPE, "code": "34133-9", "display": "Summarization of episode note" }],
                "text": "Summarization of episode note" }),
    );
    put(
        model,
        node,
        &mut body,
        "indexed",
        json!(draw.moment(602, 2018, 2026)),
    );
    put(
        model,
        node,
        &mut body,
        "subject",
        json!({ "reference": subject }),
    );
    put_any(
        model,
        node,
        &mut body,
        &["date"],
        json!(draw.moment(601, 2018, 2026)),
    );
    put(
        model,
        node,
        &mut body,
        "content",
        json!([{ "attachment": {
            "contentType": "text/plain",
            "title": "Episode summary",
            "data": "RXBpc29kZSBzdW1tYXJ5"
        }}]),
    );
    Value::Object(body)
}


pub fn subject(version: FhirVersion, seed: u64, position: u64) -> Vec<Value> {
    let model = Model::of(version);
    let draw = Draw::new(seed, position);
    let base = format!("s{seed}-{position:06}");
    let patient_id = format!("{base}-p");
    let reference = format!("Patient/{patient_id}");
    let mut held = vec![patient(model, &patient_id, seed, &draw)];
    let visits = draw.between(20, 1, 6) as usize;
    let complaints = draw.between(21, 0, 4) as usize;
    for ordinal in 0..visits {
        held.push(encounter(
            model,
            &format!("{base}-e{ordinal}"),
            &reference,
            seed,
            &draw,
            ordinal,
        ));
    }
    for ordinal in 0..complaints {
        held.push(condition(
            model,
            &format!("{base}-c{ordinal}"),
            &reference,
            seed,
            &draw,
            ordinal,
        ));
    }
    for ordinal in 0..OBSERVED.len() {
        held.push(observation(
            model,
            &format!("{base}-o{ordinal}"),
            &reference,
            seed,
            &draw,
            ordinal,
        ));
    }
    held.push(procedure(
        model,
        &format!("{base}-r"),
        &reference,
        seed,
        &draw,
    ));
    if complaints > 0 {
        held.push(medication(
            model,
            &format!("{base}-m"),
            &reference,
            seed,
            &draw,
        ));
    }
    held.push(document(
        model,
        &format!("{base}-d"),
        &reference,
        seed,
        &draw,
    ));
    held
}



pub fn population(version: FhirVersion, count: usize, seed: u64) -> Result<String, Error> {
    let mut supply = String::new();
    for position in 0..count as u64 {
        for body in subject(version, seed, position) {
            supply.push_str(&body.to_string());
            supply.push('\n');
        }
    }
    Ok(supply)
}



#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub body: Value,
    pub deleted: bool,
    pub moment: String,
}


#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub resource_type: String,
    pub id: String,
    pub changes: Vec<Change>,
}

impl Record {
    
    pub fn current(&self) -> Option<&Value> {
        match self.changes.last() {
            Some(change) if !change.deleted => Some(&change.body),
            _ => None,
        }
    }

    pub fn deleted(&self) -> bool {
        self.changes.last().is_some_and(|change| change.deleted)
    }
}




fn stamped(draw: &Draw, salt: u64, step: usize) -> String {
    let year = 2019 + step as u64;
    let month = 1 + draw.value(salt ^ 0xc1) % 12;
    let day = 1 + draw.value(salt ^ 0xc2) % 28;
    let hour = draw.value(salt ^ 0xc3) % 24;
    let minute = draw.value(salt ^ 0xc4) % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:00Z")
}

fn with_stamp(body: &Value, moment: &str) -> Value {
    let mut next = body.clone();
    if let Some(object) = next.as_object_mut() {
        let mut meta = object.get("meta").cloned().unwrap_or_else(|| json!({}));
        if let Some(inner) = meta.as_object_mut() {
            inner.insert("lastUpdated".to_owned(), json!(moment));
        }
        object.insert("meta".to_owned(), meta);
    }
    next
}



fn reads(body: &Value, named: &str) -> Option<String> {
    let held = body.get(named)?;
    held.as_str()
        .map(str::to_owned)
        .or_else(|| held["coding"][0]["code"].as_str().map(str::to_owned))
        .or_else(|| held["code"].as_str().map(str::to_owned))
}





fn amend(model: &Model, body: &Value, draw: &Draw, step: usize, prior: &[Value]) -> Value {
    let mut next = body.clone();
    let Some(node) = body["resourceType"].as_str() else {
        return next;
    };
    let salt = 700 + step as u64;
    for (named, system) in [("status", ""), ("clinicalStatus", CONDITION_CLINICAL)] {
        let Some(field) = model.field(node, named) else {
            continue;
        };
        let mut admitted: Vec<String> = field.codes().to_vec();
        if admitted.is_empty() && !system.is_empty() {
            if let Some(held) = Catalogue::of(model.version()).system(system, None) {
                admitted = terminology::flattened(&terminology::concepts(held))
                    .into_iter()
                    .map(|coding| coding.code)
                    .collect();
            }
        }
        let spent: Vec<String> = prior
            .iter()
            .chain(std::iter::once(body))
            .filter_map(|held| reads(held, named))
            .collect();
        let choices: Vec<&String> = admitted
            .iter()
            .filter(|code| !spent.contains(code))
            .collect();
        let Some(code) = choices.first() else {
            continue;
        };
        let Some(written) = shaped(model, node, named, system, code, code) else {
            continue;
        };
        if let Some(object) = next.as_object_mut() {
            object.insert(named.to_owned(), written);
            return Value::Object(object.clone());
        }
    }
    for named in [
        "onsetDateTime",
        "effectiveDateTime",
        "authoredOn",
        "performedDateTime",
        "occurrenceDateTime",
        "date",
    ] {
        let Some(held) = body[named].as_str() else {
            continue;
        };
        let Some(rest) = held.get(4..) else {
            continue;
        };
        let year = held[..4].parse::<u64>().unwrap_or(2020) + 1 + step as u64;
        if let Some(object) = next.as_object_mut() {
            object.insert(named.to_owned(), json!(format!("{year:04}{rest}")));
            return Value::Object(object.clone());
        }
    }
    if let Some(object) = next.as_object_mut() {
        if object.contains_key("telecom") {
            object.insert(
                "telecom".to_owned(),
                json!([{
                    "system": "phone",
                    "value": format!("+1-555-{:04}", draw.value(salt) % 10_000),
                    "use": "work"
                }]),
            );
        }
    }
    next
}





fn movement(draw: &Draw, ordinal: u64) -> (usize, bool) {
    let roll = draw.value(800 + ordinal) % 10;
    let versions = match roll {
        0..=5 => 1,
        6..=8 => 2,
        _ => 3,
    };
    let deleted = draw.value(900 + ordinal).is_multiple_of(10);
    (versions, deleted)
}


pub fn history(version: FhirVersion, seed: u64, position: u64) -> Vec<Record> {
    let model = Model::of(version);
    let draw = Draw::new(seed, position);
    let mut held = Vec::new();
    for (ordinal, body) in subject(version, seed, position).into_iter().enumerate() {
        let resource_type = body["resourceType"].as_str().unwrap_or_default().to_owned();
        let id = body["id"].as_str().unwrap_or_default().to_owned();
        let root = resource_type == "Patient";
        let (versions, deleted) = movement(&draw, ordinal as u64);
        let mut changes = Vec::new();
        let mut current = body;
        for step in 0..versions {
            if step > 0 {
                let prior: Vec<Value> = changes
                    .iter()
                    .map(|change: &Change| change.body.clone())
                    .collect();
                current = amend(model, &current, &draw, step, &prior);
            }
            let moment = stamped(&draw, ordinal as u64, step);
            changes.push(Change {
                body: with_stamp(&current, &moment),
                deleted: false,
                moment,
            });
        }
        if deleted && !root {
            let moment = stamped(&draw, ordinal as u64, versions);
            changes.push(Change {
                body: with_stamp(&current, &moment),
                deleted: true,
                moment,
            });
        }
        held.push(Record {
            resource_type,
            id,
            changes,
        });
    }
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_emitted_body_satisfies_the_definitions_of_every_release() {
        for version in FhirVersion::ALL {
            let model = Model::of(version);
            for position in 0..12u64 {
                for body in subject(version, 42, position) {
                    let findings = model.check(&body);
                    assert!(
                        findings.is_empty(),
                        "{version} rejected a generated body: {findings:?} in {body}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_bound_element_is_filled_from_the_binding_the_definitions_carry() {
        for version in FhirVersion::ALL {
            let model = Model::of(version);
            let admitted = model
                .field("Observation", "status")
                .expect("Observation carries a status");
            for position in 0..8u64 {
                let bodies = subject(version, 7, position);
                let observation = bodies
                    .iter()
                    .find(|body| body["resourceType"] == "Observation")
                    .expect("the subject carries an observation");
                let status = observation["status"].as_str().expect("a status is written");
                assert!(
                    admitted.codes().iter().any(|code| code == status),
                    "{version} status {status} is outside the binding"
                );
            }
        }
    }

    #[test]
    fn an_unbound_element_yields_no_code() {
        let model = Model::of(FhirVersion::R4);
        let draw = Draw::new(1, 0);
        assert!(coded(model, "Observation", "subject", &[], &draw, 0).is_none());
        assert!(coded(model, "Observation", "notAnElement", &[], &draw, 0).is_none());
    }

    #[test]
    fn a_seed_fixes_every_byte_and_a_different_seed_does_not() {
        let left = population(FhirVersion::R4, 4, 3).expect("a population generates");
        assert_eq!(left, population(FhirVersion::R4, 4, 3).expect("again"));
        assert_ne!(
            left,
            population(FhirVersion::R4, 4, 4).expect("another seed")
        );
        assert!(
            left.lines().count() > 4 * OBSERVED.len(),
            "each subject carries at least its observations"
        );
        assert!(population(FhirVersion::R4, 0, 3).expect("empty").is_empty());
    }

    #[test]
    fn the_records_name_the_patient_the_run_produced() {
        let bodies = subject(FhirVersion::R4, 11, 0);
        let patient = bodies[0]["id"].as_str().expect("a patient id");
        let reference = format!("Patient/{patient}");
        for body in bodies.iter().skip(1) {
            assert_eq!(
                body["subject"]["reference"].as_str(),
                Some(reference.as_str()),
                "every record names the subject the run produced"
            );
        }
    }

    
    
    
    
    #[test]
    fn no_release_silently_loses_what_a_record_is_for() {
        for version in FhirVersion::ALL {
            for position in 0..6u64 {
                for body in subject(version, 5, position) {
                    let kind = body["resourceType"].as_str().expect("a type");
                    let carries = |named: &str| !body[named].is_null();
                    let dated = match kind {
                        "Observation" => carries("effectiveDateTime"),
                        "Procedure" => {
                            carries("performedDateTime") || carries("occurrenceDateTime")
                        }
                        "Encounter" => carries("period") || carries("actualPeriod"),
                        "Condition" => carries("onsetDateTime"),
                        "MedicationRequest" => carries("authoredOn"),
                        "DocumentReference" => carries("date") || carries("indexed"),
                        _ => carries("birthDate"),
                    };
                    assert!(dated, "{version} {kind} lost the time it happened: {body}");
                    if kind != "Patient" {
                        assert!(carries("subject"), "{version} {kind} lost its subject");
                        let named = match kind {
                            "DocumentReference" => carries("type"),
                            "Encounter" => carries("class"),
                            "MedicationRequest" => {
                                carries("medicationCodeableConcept") || carries("medication")
                            }
                            _ => carries("code"),
                        };

                        assert!(named, "{version} {kind} lost what it records: {body}");
                    }
                }
            }
        }
    }

    
    
    
    #[test]
    fn every_emitted_body_passes_validate_on_every_release() {
        use fhir_core::validate::{validate, Mode, Request};
        use fhir_core::ResourceType;

        for version in FhirVersion::ALL {
            for position in 0..8u64 {
                for body in subject(version, 13, position) {
                    let named = body["resourceType"].as_str().expect("a type");
                    let resource_type = named
                        .parse::<ResourceType>()
                        .expect("the generator emits a known type");
                    let report = validate(&Request {
                        version,
                        resource_type: Some(resource_type),
                        id: None,
                        profile: None,
                        resolved: None,
                        mode: Mode::Update,
                        body: &body,
                    });
                    assert!(
                        !report.has_errors(),
                        "{version} $validate refused a generated {named}: {:?}",
                        report.issues()
                    );
                }
            }
        }
    }

    
    
    
    #[test]
    fn every_required_element_the_release_declares_is_present() {
        for version in FhirVersion::ALL {
            let model = Model::of(version);
            for position in 0..10u64 {
                for body in subject(version, 17, position) {
                    let node = body["resourceType"].as_str().expect("a type");
                    for named in model.elements(node) {
                        let Some(field) = model.field(node, named) else {
                            continue;
                        };
                        if !field.required() {
                            continue;
                        }
                        let written = body
                            .as_object()
                            .is_some_and(|object| object.keys().any(|key| key.starts_with(named)));
                        assert!(
                            written,
                            "{version} {node} omits the required {named}: {body}"
                        );
                    }
                }
            }
        }
    }

    
    
    
    #[test]
    fn the_cohort_spreads_rather_than_repeating_one_patient() {
        let mut years = std::collections::BTreeSet::new();
        let mut genders = std::collections::BTreeSet::new();
        let mut families = std::collections::BTreeSet::new();
        let mut sizes = std::collections::BTreeSet::new();
        for position in 0..60u64 {
            let bodies = subject(FhirVersion::R4, 23, position);
            sizes.insert(bodies.len());
            let patient = &bodies[0];
            let birth = patient["birthDate"].as_str().expect("a birth date");
            years.insert(birth[..4].to_owned());
            genders.insert(patient["gender"].as_str().unwrap_or("?").to_owned());
            families.insert(
                patient["name"][0]["family"]
                    .as_str()
                    .unwrap_or("?")
                    .to_owned(),
            );
        }
        assert!(
            years.len() > 20,
            "birth years bunch up: {} seen",
            years.len()
        );
        assert!(genders.len() >= 3, "gender barely varies: {genders:?}");
        assert!(families.len() >= 8, "names barely vary: {families:?}");
        assert!(
            sizes.len() > 1,
            "every subject carries the same number of records: {sizes:?}"
        );
    }

    
    #[test]
    fn no_reference_points_outside_the_run_that_made_it() {
        for version in FhirVersion::ALL {
            for position in 0..10u64 {
                let bodies = subject(version, 29, position);
                let held: std::collections::BTreeSet<String> = bodies
                    .iter()
                    .map(|body| {
                        format!(
                            "{}/{}",
                            body["resourceType"].as_str().unwrap_or_default(),
                            body["id"].as_str().unwrap_or_default()
                        )
                    })
                    .collect();
                for body in &bodies {
                    let Some(reference) = body["subject"]["reference"].as_str() else {
                        continue;
                    };
                    assert!(
                        held.contains(reference),
                        "{version} {} points at {reference}, which the run did not make",
                        body["resourceType"]
                    );
                }
            }
        }
    }

    
    
    
    #[test]
    fn each_record_lands_in_the_patient_compartment_it_was_built_for() {
        use fhir_core::search::compartment::{contains, Compartment};
        use fhir_core::{ResourceId, ResourceType};

        for version in FhirVersion::ALL {
            for position in 0..8u64 {
                let bodies = subject(version, 31, position);
                let patient_id = bodies[0]["id"].as_str().expect("a patient id");
                let compartment = Compartment {
                    kind: "Patient".parse::<ResourceType>().expect("a type"),
                    id: patient_id.parse::<ResourceId>().expect("an id"),
                };
                for body in bodies.iter().skip(1) {
                    let named = body["resourceType"].as_str().expect("a type");
                    let resource_type = named.parse::<ResourceType>().expect("a known type");
                    if !resolvable(resource_type) {
                        continue;
                    }
                    assert!(
                        contains(&compartment, resource_type, body),
                        "{version} {named} falls outside the compartment of {patient_id}: {body}"
                    );
                }
            }
        }
    }

    
    
    
    #[test]
    fn a_code_is_checked_against_the_catalogue_wherever_it_is_carried() {
        for version in FhirVersion::ALL {
            for (code, _display) in CLASSES {
                assert!(
                    defines(version, ACT_CODE, code),
                    "{version} does not define {code} in v3-ActCode"
                );
            }
            assert!(
                !defines(version, ACT_CODE, "NOT-A-CLASS"),
                "the catalogue would admit anything"
            );
            assert!(
                !defines(version, LOINC, "29463-7"),
                "LOINC content is not carried, so it cannot be confirmed"
            );
        }
    }

    
    
    #[test]
    fn conditions_both_resolve_and_persist() {
        let mut resolved = 0usize;
        let mut active = 0usize;
        for position in 0..80u64 {
            for body in subject(FhirVersion::R4, 37, position) {
                if body["resourceType"] != "Condition" {
                    continue;
                }
                match body["abatementDateTime"].is_null() {
                    true => active += 1,
                    false => resolved += 1,
                }
            }
        }
        assert!(
            resolved > 0 && active > 0,
            "resolved {resolved}, active {active}"
        );
    }

    
    #[test]
    fn encounters_spread_over_years() {
        let mut years = std::collections::BTreeSet::new();
        for position in 0..40u64 {
            for body in subject(FhirVersion::R4, 41, position) {
                if body["resourceType"] != "Encounter" {
                    continue;
                }
                let start = body["period"]["start"].as_str().expect("a start");
                years.insert(start[..4].to_owned());
            }
        }
        assert!(years.len() >= 5, "encounters bunch into {years:?}");
    }

    
    
    
    fn resolvable(resource_type: fhir_core::ResourceType) -> bool {
        use fhir_core::search::{compartment::definition, lookup};
        definition("Patient")
            .and_then(|def| def.member(resource_type))
            .is_some_and(|member| {
                member
                    .params
                    .iter()
                    .any(|name| lookup(Some(resource_type), name).is_some())
            })
    }

    
    
    
    
    
    
    
    
    
    
    
    
    
    
    #[test]
    fn the_types_whose_membership_cannot_be_decided_are_named_not_hidden() {
        use fhir_core::ResourceType;

        let mut undecidable = std::collections::BTreeSet::new();
        for position in 0..20u64 {
            for body in subject(FhirVersion::R4, 43, position).iter().skip(1) {
                let named = body["resourceType"].as_str().expect("a type");
                let resource_type = named.parse::<ResourceType>().expect("a known type");
                if !resolvable(resource_type) {
                    undecidable.insert(named.to_owned());
                }
            }
        }
        let named: Vec<&str> = undecidable.iter().map(String::as_str).collect();
        assert!(
            named.is_empty(),
            "these types' compartment membership still cannot be decided: {named:?}"
        );
    }

    
    
    #[test]
    fn every_version_of_every_record_still_satisfies_its_release() {
        for version in FhirVersion::ALL {
            let model = Model::of(version);
            for position in 0..10u64 {
                for record in history(version, 47, position) {
                    for change in &record.changes {
                        let findings = model.check(&change.body);
                        assert!(
                            findings.is_empty(),
                            "{version} rejected version of {}: {findings:?}",
                            record.id
                        );
                    }
                }
            }
        }
    }

    
    
    
    #[test]
    fn a_records_versions_are_stamped_in_the_order_they_happened() {
        for version in FhirVersion::ALL {
            for position in 0..12u64 {
                for record in history(version, 53, position) {
                    let moments: Vec<&str> =
                        record.changes.iter().map(|c| c.moment.as_str()).collect();
                    let mut sorted = moments.clone();
                    sorted.sort_unstable();
                    assert_eq!(
                        moments, sorted,
                        "{} is stamped out of order: {moments:?}",
                        record.id
                    );
                    for change in &record.changes {
                        assert_eq!(
                            change.body["meta"]["lastUpdated"].as_str(),
                            Some(change.moment.as_str()),
                            "the body of {} does not carry its own moment",
                            record.id
                        );
                    }
                }
            }
        }
    }

    
    
    #[test]
    fn the_population_holds_records_written_once_amended_and_deleted() {
        let mut once = 0usize;
        let mut amended = 0usize;
        let mut deleted = 0usize;
        for position in 0..60u64 {
            for record in history(FhirVersion::R4, 59, position) {
                if record.deleted() {
                    deleted += 1;
                } else if record.changes.len() > 1 {
                    amended += 1;
                } else {
                    once += 1;
                }
            }
        }
        assert!(once > 0, "nothing was written once");
        assert!(amended > 0, "nothing was amended");
        assert!(deleted > 0, "nothing was deleted");
        assert!(
            once > amended + deleted,
            "once {once}, amended {amended}, deleted {deleted}"
        );
    }

    
    #[test]
    fn an_amendment_changes_something_a_reader_would_see() {
        for version in FhirVersion::ALL {
            for position in 0..20u64 {
                for record in history(version, 61, position) {
                    for pair in record.changes.windows(2) {
                        if pair[1].deleted {
                            continue;
                        }
                        let (before, after) = (&pair[0].body, &pair[1].body);
                        let differs = before.as_object().zip(after.as_object()).is_some_and(
                            |(left, right)| {
                                left.iter().any(|(key, value)| {
                                    key != "meta" && right.get(key) != Some(value)
                                })
                            },
                        );
                        assert!(
                            differs,
                            "a version of {} changed nothing but its stamp",
                            record.id
                        );
                    }
                }
            }
        }
    }

    
    
    #[test]
    fn the_compartment_root_is_never_deleted() {
        for version in FhirVersion::ALL {
            for position in 0..40u64 {
                for record in history(version, 67, position) {
                    if record.resource_type == "Patient" {
                        assert!(!record.deleted(), "{} was deleted", record.id);
                    }
                }
            }
        }
    }

    #[test]
    fn a_seed_fixes_the_history_as_it_fixes_the_bodies() {
        let left = history(FhirVersion::R4, 71, 3);
        assert_eq!(left, history(FhirVersion::R4, 71, 3));
        assert_ne!(left, history(FhirVersion::R4, 72, 3));
        assert!(left.iter().all(|record| !record.changes.is_empty()));
    }

    
    
    
    #[test]
    fn no_record_returns_to_a_state_it_already_held() {
        for version in FhirVersion::ALL {
            for position in 0..20u64 {
                for record in history(version, 73, position) {
                    let mut seen = Vec::new();
                    for change in &record.changes {
                        if change.deleted {
                            continue;
                        }
                        let mut body = change.body.clone();
                        if let Some(object) = body.as_object_mut() {
                            object.remove("meta");
                        }
                        assert!(
                            !seen.contains(&body),
                            "{version} {} returns to a state it already held",
                            record.id
                        );
                        seen.push(body);
                    }
                }
            }
        }
    }
}
