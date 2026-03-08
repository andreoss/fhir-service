use fhir_core::{Error, ResourceType};
use serde_json::{json, Value};

const MIX: u64 = 0x9e37_79b9_7f4a_7c15;

fn mixed(seed: u64, position: u64) -> u64 {
    let mut value = seed.wrapping_add(position.wrapping_mul(MIX));
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn day(value: u64) -> String {
    let year = 1930 + (value % 80);
    let month = 1 + ((value >> 8) % 12);
    let day = 1 + ((value >> 16) % 28);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn rows(count: usize, label: &str, seed: u64) -> Result<String, Error> {
    let resource_type = label.parse::<ResourceType>()?;
    let name = resource_type.as_str();
    let mut supply = String::new();
    for position in 0..count as u64 {
        let value = mixed(seed, position);
        let row = json!({
            "resourceType": name,
            "id": format!("gen-{seed}-{position:06}"),
            "meta": { "tag": [{ "system": "urn:generated", "code": format!("{seed}") }] },
            "identifier": [{ "system": "urn:generated", "value": format!("{value:016x}") }],
            "date": day(value),
        });
        supply.push_str(&row.to_string());
        supply.push('\n');
    }
    Ok(supply)
}

pub const COHORT_SHARE: usize = 5;

const FAMILY: [&str; 8] = [
    "Abrams",
    "Bennett",
    "Castillo",
    "Delaney",
    "Eriksen",
    "Fournier",
    "Ghosh",
    "Halvorsen",
];
const GIVEN: [&str; 8] = [
    "Aurelia", "Benedict", "Camille", "Dimitri", "Elowen", "Fabian", "Giselle", "Hector",
];
const CITY: [&str; 4] = ["Ashford", "Brookvale", "Carrowden", "Dunmoor"];
const GENDER: [&str; 4] = ["female", "male", "other", "unknown"];
const CONTACT: [&str; 2] = ["home", "work"];
const OBSERVED: [(&str, &str, &str, &str); 3] = [
    ("29463-7", "Body weight", "kg", "kg"),
    ("8867-4", "Heart rate", "beats/minute", "/min"),
    ("8480-6", "Systolic blood pressure", "mmHg", "mm[Hg]"),
];
const PROCEDURES: [(&str, &str); 4] = [
    ("80146002", "Appendicectomy"),
    ("274031008", "Rectal examination"),
    ("35637008", "Blood transfusion"),
    ("18286008", "Wound dressing"),
];
const LOINC: &str = "http://loinc.org";
const SNOMED: &str = "http://snomed.info/sct";
const UCUM: &str = "http://unitsofmeasure.org";

fn instant(value: u64) -> String {
    let hour = value % 24;
    let minute = (value >> 8) % 60;
    let second = (value >> 16) % 60;
    format!("{}T{hour:02}:{minute:02}:{second:02}Z", day(value))
}

fn patient(id: &str, seed: u64, value: u64) -> Value {
    let family = FAMILY[(value % FAMILY.len() as u64) as usize];
    let given = GIVEN[((value >> 3) % GIVEN.len() as u64) as usize];
    let city = CITY[((value >> 6) % CITY.len() as u64) as usize];
    json!({
        "resourceType": "Patient",
        "id": id,
        "meta": { "tag": [{ "system": "urn:generated", "code": format!("{seed}") }] },
        "identifier": [
            { "system": "urn:generated:record", "value": format!("{value:016x}") },
            { "system": "urn:generated:member", "value": format!("{:08}", value % 100_000_000) }
        ],
        "active": !value.is_multiple_of(8),
        "name": [{ "use": "official", "family": family, "given": [given, GIVEN[((value >> 9) % GIVEN.len() as u64) as usize]] }],
        "telecom": [
            { "system": "phone", "value": format!("+1-555-{:04}", value % 10_000), "use": CONTACT[(value % 2) as usize] },
            { "system": "email", "value": format!("{}.{}@example.invalid", given.to_lowercase(), family.to_lowercase()), "use": "home" }
        ],
        "gender": GENDER[((value >> 12) % 4) as usize],
        "birthDate": day(value >> 4),
        "address": [{
            "use": "home",
            "line": [format!("{} {} Street", value % 400, family)],
            "city": city,
            "postalCode": format!("{:05}", value % 100_000),
            "country": "GB"
        }]
    })
}

fn observation(id: &str, subject: &str, seed: u64, value: u64, ordinal: usize) -> Value {
    let (code, display, unit, symbol) = OBSERVED[ordinal % OBSERVED.len()];
    let reading = 40 + (value >> (ordinal * 5)) % 120;
    json!({
        "resourceType": "Observation",
        "id": id,
        "meta": { "tag": [{ "system": "urn:generated", "code": format!("{seed}") }] },
        "status": "final",
        "category": [{ "coding": [{
            "system": "http://terminology.hl7.org/CodeSystem/observation-category",
            "code": "vital-signs",
            "display": "Vital Signs"
        }] }],
        "code": { "coding": [{ "system": LOINC, "code": code, "display": display }], "text": display },
        "subject": { "reference": subject },
        "effectiveDateTime": instant(value.wrapping_add(ordinal as u64)),
        "valueQuantity": { "value": reading, "unit": unit, "system": UCUM, "code": symbol }
    })
}

fn procedure(id: &str, subject: &str, seed: u64, value: u64) -> Value {
    let (code, display) = PROCEDURES[((value >> 20) % PROCEDURES.len() as u64) as usize];
    json!({
        "resourceType": "Procedure",
        "id": id,
        "meta": { "tag": [{ "system": "urn:generated", "code": format!("{seed}") }] },
        "status": "completed",
        "code": { "coding": [{ "system": SNOMED, "code": code, "display": display }], "text": display },
        "subject": { "reference": subject },
        "note": [{ "text": format!("{display} recorded on {}", day(value >> 2)) }]
    })
}

pub fn cohort(count: usize, seed: u64) -> Result<String, Error> {
    let mut supply = String::new();
    for position in 0..count as u64 {
        let value = mixed(seed, position);
        let base = format!("c{seed}-{position:06}");
        let subject = format!("Patient/{base}-p");
        let mut rows = vec![patient(&format!("{base}-p"), seed, value)];
        for ordinal in 0..3 {
            rows.push(observation(
                &format!("{base}-o{ordinal}"),
                &subject,
                seed,
                value,
                ordinal,
            ));
        }
        rows.push(procedure(&format!("{base}-r"), &subject, seed, value));
        for row in rows {
            supply.push_str(&row.to_string());
            supply.push('\n');
        }
    }
    Ok(supply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_fixes_every_row() {
        let left = rows(3, "Patient", 1).expect("a supply generates");
        assert_eq!(left, rows(3, "Patient", 1).expect("a supply generates"));
        assert_ne!(left, rows(3, "Patient", 2).expect("a supply generates"));
        assert_eq!(left.lines().count(), 3);
    }

    #[test]
    fn a_generated_day_is_a_day() {
        for position in 0..64u64 {
            let text = day(mixed(5, position));
            let parts: Vec<&str> = text.split('-').collect();
            assert_eq!(parts.len(), 3);
            assert!((1..=12).contains(&parts[1].parse::<u32>().expect("a month")));
            assert!((1..=28).contains(&parts[2].parse::<u32>().expect("a day")));
        }
    }

    #[test]
    fn an_unknown_type_is_refused() {
        assert!(rows(1, "Nonsense", 1).is_err());
        assert_eq!(
            rows(0, "Patient", 1).expect("an empty supply"),
            String::new()
        );
    }
}
