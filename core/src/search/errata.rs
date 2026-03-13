use crate::search::published::Published;

const GROUPED: &str = "DeviceUseStatement";

const GROUP: &str = "Group";

pub fn corrected(held: &mut Published) -> bool {
    patient_reaches_a_patient(held);
    held.paths.retain(|path| !names_a_type(path));
    context_names_the_expression(held);
    !held.paths.is_empty()
}

fn patient_reaches_a_patient(held: &mut Published) {
    if held.name != "patient" || held.kind == GROUPED {
        return;
    }
    held.targets.retain(|target| target != GROUP);
}

fn names_a_type(path: &str) -> bool {
    path.split('.')
        .next()
        .and_then(|first| first.chars().next())
        .is_some_and(char::is_uppercase)
}

fn context_names_the_expression(held: &mut Published) {
    if held.kind != "StructureDefinition" || held.name != "ext-context" {
        return;
    }
    for path in held.paths.iter_mut() {
        if path == "context" {
            *path = "context.expression".to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::value::ValueType;
    use crate::FhirVersion;

    fn parameter(kind: &str, name: &str, paths: &[&str], targets: &[&str]) -> Published {
        Published {
            kind: kind.to_owned(),
            name: name.to_owned(),
            value_type: ValueType::Reference,
            paths: paths.iter().map(|held| (*held).to_owned()).collect(),
            targets: targets.iter().map(|held| (*held).to_owned()).collect(),
            since: FhirVersion::Stu3,
            until: None,
        }
    }

    #[test]
    fn a_clinical_patient_parameter_loses_the_group() {
        let mut held = parameter("Condition", "patient", &["subject"], &["Patient", "Group"]);
        assert!(corrected(&mut held));
        assert_eq!(held.targets, vec!["Patient".to_owned()]);
    }

    #[test]
    fn the_one_type_that_meant_it_keeps_it() {
        let mut held = parameter(
            "DeviceUseStatement",
            "patient",
            &["subject"],
            &["Patient", "Group"],
        );
        assert!(corrected(&mut held));
        assert_eq!(held.targets, vec!["Patient".to_owned(), "Group".to_owned()]);
    }

    #[test]
    fn a_path_rooted_at_a_type_is_dropped() {
        let mut held = parameter("Observation", "x", &["Resource.meta.lastUpdated"], &[]);
        assert!(
            !corrected(&mut held),
            "nothing is left to index, so the parameter is not carried"
        );
    }

    #[test]
    fn a_path_rooted_at_a_type_beside_a_good_one_leaves_the_good_one() {
        let mut held = parameter("Observation", "x", &["Resource.meta.tag", "meta.tag"], &[]);
        assert!(corrected(&mut held));
        assert_eq!(held.paths, vec!["meta.tag".to_owned()]);
    }

    #[test]
    fn the_context_parameter_is_moved_off_the_backbone() {
        let mut held = parameter("StructureDefinition", "ext-context", &["context"], &[]);
        assert!(corrected(&mut held));
        assert_eq!(held.paths, vec!["context.expression".to_owned()]);
    }

    #[test]
    fn a_parameter_with_nothing_wrong_with_it_is_unchanged() {
        let held = parameter(
            "Observation",
            "subject",
            &["subject"],
            &["Patient", "Group"],
        );
        let mut through = held.clone();
        assert!(corrected(&mut through));
        assert_eq!(through, held, "subject may reach a group and always could");
    }
}
