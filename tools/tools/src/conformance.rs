use crate::http::send;
use fhir_core::Error;
use std::path::Path;

pub struct Exchange {
    pub name: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub body: &'static str,
}

const PATIENT: &str = r#"{"resourceType":"Patient","id":"p1","active":true,
"name":[{"family":"Held","given":["A"]}],"gender":"female","birthDate":"1980-02-03"}"#;

const OBSERVATION: &str = r#"{"resourceType":"Observation","id":"o1","status":"final",
"code":{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/observation-category",
"code":"vital-signs"}]},"subject":{"reference":"Patient/p1"},
"valueQuantity":{"value":9.0,"unit":"kg"}}"#;

const VALUE_SET: &str = r#"{"resourceType":"ValueSet","id":"vs1","url":"urn:vs-external",
"status":"active","compose":{"include":[{"system":
"http://terminology.hl7.org/CodeSystem/v3-Confidentiality",
"filter":[{"property":"concept","op":"is-a","value":"_Confidentiality"}]}]}}"#;

const CONDITION: &str = r#"{"resourceType":"Condition","id":"c1","clinicalStatus":{"coding":
[{"system":"http://terminology.hl7.org/CodeSystem/condition-clinical","code":"active"}]},
"verificationStatus":{"coding":[{"system":
"http://terminology.hl7.org/CodeSystem/condition-ver-status","code":"confirmed"}]},
"category":[{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/condition-category",
"code":"encounter-diagnosis"}]}],"code":{"coding":[{"system":"http://snomed.info/sct",
"code":"38341003","display":"Hypertension"}],"text":"Hypertension"},
"subject":{"reference":"Patient/p1"}}"#;

const HEART_RATE: &str = r#"{"resourceType":"Observation","id":"o2","status":"final",
"category":[{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/observation-category",
"code":"vital-signs"}]}],"code":{"coding":[{"system":"http://loinc.org","code":"8867-4",
"display":"Heart rate"}],"text":"Heart rate"},"subject":{"reference":"Patient/p1"},
"effectiveDateTime":"2026-09-08T10:00:00Z","valueQuantity":{"value":72.0,
"unit":"beats/minute","system":"http://unitsofmeasure.org","code":"/min"}}"#;

pub const EXCHANGES: [Exchange; 11] = [
    Exchange {
        name: "capability",
        method: "GET",
        path: "/metadata",
        body: "",
    },
    Exchange {
        name: "created-patient",
        method: "POST",
        path: "/Patient",
        body: PATIENT,
    },
    Exchange {
        name: "created-observation",
        method: "POST",
        path: "/Observation",
        body: OBSERVATION,
    },
    Exchange {
        name: "created-valueset",
        method: "POST",
        path: "/ValueSet",
        body: VALUE_SET,
    },
    Exchange {
        name: "created-condition",
        method: "POST",
        path: "/Condition",
        body: CONDITION,
    },
    Exchange {
        name: "created-heart-rate",
        method: "POST",
        path: "/Observation",
        body: HEART_RATE,
    },
    Exchange {
        name: "searched-bundle",
        method: "GET",
        path: "/Patient?name=Held",
        body: "",
    },
    Exchange {
        name: "history-bundle",
        method: "GET",
        path: "/Patient/_history",
        body: "",
    },
    Exchange {
        name: "outcome",
        method: "GET",
        path: "/Patient/nonesuch",
        body: "",
    },
    Exchange {
        name: "expansion",
        method: "GET",
        path: "/ValueSet/$expand?url=urn:vs-external&count=5",
        body: "",
    },
    Exchange {
        name: "everything",
        method: "GET",
        path: "/Patient/p1/$everything",
        body: "",
    },
];

pub fn capture(address: &str, host: &str, into: &Path) -> Result<Vec<String>, Error> {
    std::fs::create_dir_all(into).map_err(|reason| Error::Config(reason.to_string()))?;
    let mut written = Vec::new();
    for exchange in &EXCHANGES {
        let (status, body) = send(address, host, exchange.method, exchange.path, exchange.body)?;
        let path = into.join(format!("{}.json", exchange.name));
        std::fs::write(&path, body.as_bytes())
            .map_err(|reason| Error::Config(format!("{}: {reason}", path.display())))?;
        written.push(format!("{} {status}", exchange.name));
    }
    Ok(written)
}
