use fhir_core::fhir_version::FhirVersion;
use fhir_core::xml::{from_xml, to_xml, to_xml_pretty};
use serde_json::{json, Value};

const PATIENT: &str = r#"{"resourceType":"Patient","id":"example","text":{"status":"generated","div":"<div xmlns=\"http://www.w3.org/1999/xhtml\"><p>Ann <b>Smith</b></p></div>"},"active":true,"name":[{"family":"Smith","given":["Ann"]}],"deceasedBoolean":false,"extension":[{"url":"http://example.org/ext","valueString":"note"}]}"#;

const PATIENT_XML: &str = concat!(
    r#"<Patient xmlns="http://hl7.org/fhir">"#,
    r#"<id value="example"/>"#,
    r#"<text><status value="generated"/><div xmlns="http://www.w3.org/1999/xhtml"><p>Ann <b>Smith</b></p></div></text>"#,
    r#"<extension url="http://example.org/ext"><valueString value="note"/></extension>"#,
    r#"<active value="true"/>"#,
    r#"<name><family value="Smith"/><given value="Ann"/></name>"#,
    r#"<deceasedBoolean value="false"/>"#,
    r#"</Patient>"#,
);

fn body(text: &str) -> Value {
    serde_json::from_str(text).expect("the fixture is json")
}

#[test]
fn a_patient_is_written_as_the_published_representation_does() {
    let written = to_xml(FhirVersion::R4, &body(PATIENT)).expect("the patient is written");
    assert_eq!(written, PATIENT_XML);
}

#[test]
fn a_patient_read_back_is_the_one_that_was_written() {
    let read = from_xml(FhirVersion::R4, PATIENT_XML).expect("the patient is read");
    assert_eq!(read, body(PATIENT));
}

#[test]
fn every_version_round_trips_its_own_resource() {
    for version in FhirVersion::ALL {
        let written = to_xml(version, &body(PATIENT)).expect("the patient is written");
        let read = from_xml(version, &written).expect("the patient is read back");
        assert_eq!(read, body(PATIENT), "{version} changed the resource");
    }
}

#[test]
fn a_bundle_keeps_its_entries_and_the_resources_they_carry() {
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "searchset",
        "total": 1,
        "entry": [{
            "fullUrl": "http://example.org/Patient/example",
            "resource": {"resourceType": "Patient", "id": "example", "active": true},
        }],
    });
    let written = to_xml(FhirVersion::R4, &bundle).expect("the bundle is written");
    assert!(written.contains("<entry><fullUrl value=\"http://example.org/Patient/example\"/><resource><Patient><id value=\"example\"/><active value=\"true\"/></Patient></resource></entry>"), "{written}");
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the bundle is read"),
        bundle
    );
}

#[test]
fn numbers_booleans_and_strings_keep_the_shape_json_gives_them() {
    let observation = json!({
        "resourceType": "Observation",
        "status": "final",
        "valueQuantity": {"value": 3.5, "unit": "kg"},
        "code": {"coding": [{"code": "29463-7"}]},
    });
    let written = to_xml(FhirVersion::R4, &observation).expect("the observation is written");
    assert!(written.contains("<value value=\"3.5\"/>"));
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the observation is read"),
        observation
    );
}

#[test]
fn an_id_and_extensions_on_a_primitive_are_attributes_and_children() {
    let patient = json!({
        "resourceType": "Patient",
        "active": true,
        "_active": {"id": "a1", "extension": [{"url": "http://example.org/why", "valueString": "because"}]},
    });
    let written = to_xml(FhirVersion::R4, &patient).expect("the patient is written");
    assert!(written.contains("<active id=\"a1\" value=\"true\"><extension url=\"http://example.org/why\"><valueString value=\"because\"/></extension></active>"));
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the patient is read"),
        patient
    );
}

#[test]
fn the_narrative_is_written_and_read_as_markup() {
    let written = to_xml(FhirVersion::R4, &body(PATIENT)).expect("the patient is written");
    assert!(written
        .contains("<div xmlns=\"http://www.w3.org/1999/xhtml\"><p>Ann <b>Smith</b></p></div>"));
    let read = from_xml(FhirVersion::R4, &written).expect("the patient is read");
    assert_eq!(read["text"]["div"], body(PATIENT)["text"]["div"]);
}

#[test]
fn markup_in_a_value_is_escaped_and_read_back_unchanged() {
    let patient = json!({"resourceType": "Patient", "name": [{"family": "A & B <c> \"d\""}]});
    let written = to_xml(FhirVersion::R4, &patient).expect("the patient is written");
    assert!(written.contains("value=\"A &amp; B &lt;c&gt; &quot;d&quot;\""));
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the patient is read"),
        patient
    );
}

#[test]
fn a_contained_resource_keeps_its_own_type() {
    let patient = json!({
        "resourceType": "Patient",
        "contained": [{"resourceType": "Organization", "id": "org", "name": "Acme"}],
    });
    let written = to_xml(FhirVersion::R4, &patient).expect("the patient is written");
    assert!(written.contains("<contained><Organization><id value=\"org\"/><name value=\"Acme\"/></Organization></contained>"), "{written}");
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the patient is read"),
        patient
    );
}

#[test]
fn a_single_occurrence_of_a_repeating_element_stays_an_array() {
    let patient = json!({"resourceType": "Patient", "name": [{"family": "Smith"}]});
    let written = to_xml(FhirVersion::R4, &patient).expect("the patient is written");
    let read = from_xml(FhirVersion::R4, &written).expect("the patient is read");
    assert_eq!(read["name"], json!([{"family": "Smith"}]));
}

#[test]
fn a_choice_element_keeps_the_type_its_name_carries() {
    let patient = json!({"resourceType": "Patient", "deceasedDateTime": "2020-01-01"});
    let written = to_xml(FhirVersion::R4, &patient).expect("the patient is written");
    assert!(written.contains("<deceasedDateTime value=\"2020-01-01\"/>"));
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the patient is read"),
        patient
    );
}

#[test]
fn a_comment_and_a_declaration_are_not_content() {
    let written = to_xml(FhirVersion::R4, &body(PATIENT)).expect("the patient is written");
    let decorated =
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- a note -->\n{written}\n");
    assert_eq!(
        from_xml(FhirVersion::R4, &decorated).expect("the patient is read"),
        body(PATIENT)
    );
}

#[test]
fn malformed_documents_are_refused() {
    for text in [
        "",
        "<Patient>",
        "<Patient></Patientname>",
        "<Patient><id value=\"a\"></Patient>",
        "<Patient><active value=\"maybe\"/></Patient>",
        "<Patient><id value=\"a\"/></Patient><Patient/>",
        "<Nonesuch><id value=\"a\"/></Nonesuch>",
    ] {
        let read = from_xml(FhirVersion::R4, text);
        assert!(read.is_err(), "{text} was accepted");
    }
    assert!(to_xml(FhirVersion::R4, &json!({"id": "a"})).is_err());
    assert!(to_xml(FhirVersion::R4, &json!({"resourceType": "Nonesuch"})).is_err());
}

#[test]
fn a_published_example_is_read_and_accepted_by_the_model() {
    let published = include_str!("fixtures/patient-example.xml");
    let read = from_xml(FhirVersion::R4, published).expect("the example is read");
    assert_eq!(read["resourceType"], json!("Patient"));
    let findings = fhir_core::model::Model::of(FhirVersion::R4).check(&read);
    assert!(findings.is_empty(), "{findings:?}");
    let written = to_xml(FhirVersion::R4, &read).expect("the example is written");
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the example is read again"),
        read
    );
}

const PATIENT_XML_PRETTY: &str = r#"<Patient xmlns="http://hl7.org/fhir">
  <id value="example"/>
  <text>
    <status value="generated"/>
    <div xmlns="http://www.w3.org/1999/xhtml"><p>Ann <b>Smith</b></p></div>
  </text>
  <extension url="http://example.org/ext">
    <valueString value="note"/>
  </extension>
  <active value="true"/>
  <name>
    <family value="Smith"/>
    <given value="Ann"/>
  </name>
  <deceasedBoolean value="false"/>
</Patient>"#;

#[test]
fn a_pretty_document_is_the_same_resource_as_the_compact_one() {
    let written = to_xml_pretty(FhirVersion::R4, &body(PATIENT)).expect("the patient is written");
    assert_eq!(written, PATIENT_XML_PRETTY);
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the patient is read"),
        body(PATIENT)
    );
}

#[test]
fn every_version_writes_a_pretty_document_that_reads_back() {
    for version in FhirVersion::ALL {
        let written = to_xml_pretty(version, &body(PATIENT)).expect("the patient is written");
        assert!(
            written.contains("\n  <id value=\"example\"/>"),
            "{version} wrote {written}"
        );
        let read = from_xml(version, &written).expect("the patient is read back");
        assert_eq!(read, body(PATIENT), "{version} changed the resource");
    }
}

#[test]
fn a_pretty_document_keeps_the_shape_of_the_narrative_it_carries() {
    let narrative = json!({
        "resourceType": "Patient",
        "id": "n1",
        "text": {"status": "generated", "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">\n  <p>two lines</p>\n</div>"},
    });
    let written = to_xml_pretty(FhirVersion::R4, &narrative).expect("the patient is written");
    assert!(written.contains("\n    <div"), "{written}");
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the patient is read"),
        narrative
    );
}

#[test]
fn a_pretty_document_indents_a_resource_it_contains() {
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "searchset",
        "entry": [{
            "resource": {"resourceType": "Patient", "id": "example", "active": true},
        }],
    });
    let written = to_xml_pretty(FhirVersion::R4, &bundle).expect("the bundle is written");
    assert!(
        written.contains(concat!(
            "  <entry>\n",
            "    <resource>\n",
            "      <Patient>\n",
            "        <id value=\"example\"/>\n",
            "        <active value=\"true\"/>\n",
            "      </Patient>\n",
            "    </resource>\n",
            "  </entry>\n",
        )),
        "{written}"
    );
    assert_eq!(
        from_xml(FhirVersion::R4, &written).expect("the bundle is read"),
        bundle
    );
}
