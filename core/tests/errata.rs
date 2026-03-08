













use fhir_core::search::registry::{for_type_in, lookup, Target};
use fhir_core::search::value::ValueType;
use fhir_core::{FhirVersion, ResourceType};

fn kind(name: &str) -> ResourceType {
    name.parse().expect("a type the release serves")
}

fn paths(resource_type: &str, name: &str) -> Vec<String> {
    let def = lookup(Some(kind(resource_type)), name)
        .unwrap_or_else(|| panic!("{resource_type} publishes {name}"));
    match &def.target {
        Target::Path(paths) => paths.clone(),
        other => panic!("{resource_type}.{name} is {other:?}, not a path"),
    }
}



#[test]
fn the_patient_parameter_names_only_patient() {
    for resource_type in [
        "Observation",
        "Condition",
        "Procedure",
        "Encounter",
        "MedicationRequest",
    ] {
        let def = lookup(Some(kind(resource_type)), "patient")
            .unwrap_or_else(|| panic!("{resource_type} publishes patient"));
        assert_eq!(
            def.targets,
            vec!["Patient".to_owned()],
            "{resource_type}.patient reaches a patient and nothing else"
        );
    }
}





#[test]
fn the_date_parameters_the_erratum_names_are_dates_that_index_something() {
    let held: &[(&str, &str)] = &[
        ("Observation", "date"),
        ("Condition", "recorded-date"),
        ("DeviceRequest", "event-date"),
        ("Observation", "value-date"),
        ("Patient", "death-date"),
    ];
    for (resource_type, name) in held {
        let Some(def) = lookup(Some(kind(resource_type)), name) else {
            continue;
        };
        assert_eq!(
            def.value_type,
            ValueType::Date,
            "{resource_type}.{name} is a date"
        );
        assert!(
            !paths(resource_type, name).is_empty(),
            "{resource_type}.{name} names an element to index, which is what \
             the broken cast cost"
        );
    }
}





#[test]
fn no_parameter_path_begins_with_a_type_name() {
    for version in [
        FhirVersion::Stu3,
        FhirVersion::R4,
        FhirVersion::R4b,
        FhirVersion::R5,
    ] {
        for resource_type in ResourceType::served(version) {
            for def in for_type_in(version, resource_type) {
                let Target::Path(paths) = &def.target else {
                    continue;
                };
                for path in paths {
                    let first = path.split('.').next().unwrap_or_default();
                    assert!(
                        !first.chars().next().is_some_and(char::is_uppercase),
                        "{version:?} {resource_type}.{} indexes {path:?}, which \
                         names a type rather than an element",
                        def.name
                    );
                }
            }
        }
    }
}




#[test]
fn the_context_parameter_indexes_something_with_a_value_in_it() {
    let Some(def) = lookup(Some(kind("StructureDefinition")), "ext-context") else {
        return;
    };
    let Target::Path(paths) = &def.target else {
        panic!("ext-context is a path");
    };
    for path in paths {
        assert!(
            path.contains('.'),
            "ext-context indexes {path:?}, which is the backbone and not a \
             value within it"
        );
    }
}




#[test]
fn no_path_carries_fhirpath_syntax() {
    for resource_type in ResourceType::served(FhirVersion::R4) {
        for def in for_type_in(FhirVersion::R4, resource_type) {
            let Target::Path(paths) = &def.target else {
                continue;
            };
            for path in paths {
                for held in ["(", ")", ".as", ".where", "|", "'"] {
                    assert!(
                        !path.contains(held),
                        "{resource_type}.{} indexes {path:?}, which carries \
                         FHIRPath syntax this build does not evaluate",
                        def.name
                    );
                }
            }
        }
    }
}
