use fhir_core::convert::{convert, ApprovedTemplates, Conversion, InputType, TemplateCollection};
use fhir_core::Error;
use serde_json::json;

fn collection() -> ApprovedTemplates {
    ApprovedTemplates::new(vec![TemplateCollection::new("urn:collection:test").with(
        "Patient",
        json!({
            "resourceType": "Patient",
            "id": "{{PID.3}}",
            "name": [{"family": "{{PID.5.1}}", "given": ["{{PID.5.2}}"]}],
            "gender": "{{PID.8}}"
        }),
    )])
}

fn request<'a>(input: InputType, data: &'a str) -> Conversion<'a> {
    Conversion {
        input_type: input,
        data,
        collection: "urn:collection:test",
        root_template: "Patient",
    }
}

#[test]
fn delimited_input_renders_the_named_template() {
    let data = "MSH|^~\\&|s\rPID|1||pt-1||Ann^Bea||19800401|female";
    let value = convert(&collection(), &request(InputType::Hl7v2, data)).unwrap();
    assert_eq!(value["resourceType"], "Patient");
    assert_eq!(value["id"], "pt-1");
    assert_eq!(value["name"][0]["family"], "Ann");
    assert_eq!(value["name"][0]["given"][0], "Bea");
    assert_eq!(value["gender"], "female");
}

#[test]
fn an_unresolved_placeholder_drops_its_element() {
    let data = "PID|1||pt-2";
    let value = convert(&collection(), &request(InputType::Hl7v2, data)).unwrap();
    assert_eq!(value["id"], "pt-2");
    assert!(value.get("name").is_none());
    assert!(value.get("gender").is_none());
}

#[test]
fn json_input_binds_by_path() {
    let data = r#"{"PID": {"3": "pt-3", "5": {"1": "Cyd"}}}"#;
    let value = convert(&collection(), &request(InputType::Json, data)).unwrap();
    assert_eq!(value["id"], "pt-3");
    assert_eq!(value["name"][0]["family"], "Cyd");
}

#[test]
fn a_collection_outside_the_approved_registry_is_refused() {
    let mut asked = request(InputType::Json, "{}");
    asked.collection = "urn:collection:other";
    assert!(matches!(
        convert(&collection(), &asked).unwrap_err(),
        Error::Forbidden(_)
    ));
}

#[test]
fn an_unknown_template_is_refused() {
    let mut asked = request(InputType::Json, "{}");
    asked.root_template = "Nonesuch";
    assert!(matches!(
        convert(&collection(), &asked).unwrap_err(),
        Error::InvalidParameter(_)
    ));
}

#[test]
fn malformed_input_is_refused() {
    assert!(matches!(
        convert(&collection(), &request(InputType::Json, "not json")).unwrap_err(),
        Error::InvalidJson(_)
    ));
    assert!(matches!(
        convert(&collection(), &request(InputType::Fhir, r#"{"id":"x"}"#)).unwrap_err(),
        Error::InvalidEnvelope(_)
    ));
}

#[test]
fn an_input_form_the_server_does_not_read_is_refused() {
    assert!(matches!(
        "ccda".parse::<InputType>().unwrap_err(),
        Error::UnsupportedParameter(_)
    ));
    assert_eq!("hl7v2".parse::<InputType>().unwrap(), InputType::Hl7v2);
    assert_eq!("fhir".parse::<InputType>().unwrap(), InputType::Fhir);
    assert_eq!("json".parse::<InputType>().unwrap(), InputType::Json);
}

#[test]
fn the_default_registry_approves_one_collection() {
    let templates = ApprovedTemplates::default();
    let data = "PID|1||pt-4||Dee^Eve||19700101|male";
    let value = convert(
        &templates,
        &Conversion {
            input_type: InputType::Hl7v2,
            data,
            collection: fhir_core::convert::DEFAULT_COLLECTION,
            root_template: "Patient",
        },
    )
    .unwrap();
    assert_eq!(value["resourceType"], "Patient");
    assert_eq!(value["id"], "pt-4");
}
