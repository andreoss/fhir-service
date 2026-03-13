use fhir_core::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    pub name: String,
    pub run: String,
    pub gate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub stages: Vec<Stage>,
}

fn value(line: &str, key: &str) -> Option<String> {
    line.strip_prefix(key)
        .and_then(|rest| rest.strip_prefix(':'))
        .map(|held| held.trim().to_owned())
}

impl Pipeline {
    pub fn parse(text: &str) -> Result<Pipeline, Error> {
        let refused = |reason: &str| Error::Config(format!("the pipeline definition {reason}"));
        let mut stages: Vec<Stage> = Vec::new();
        let mut name: Option<String> = None;
        let mut run: Option<String> = None;
        let mut gate: Option<f64> = None;
        let close = |name: &mut Option<String>,
                     run: &mut Option<String>,
                     gate: &mut Option<f64>,
                     stages: &mut Vec<Stage>|
         -> Result<(), Error> {
            match (name.take(), run.take()) {
                (None, None) => Ok(()),
                (Some(name), Some(run)) => {
                    stages.push(Stage {
                        name,
                        run,
                        gate: gate.take(),
                    });
                    Ok(())
                }
                _ => Err(Error::Config(
                    "the pipeline definition holds a stage without a name and a command".to_owned(),
                )),
            }
        };
        for line in text.lines() {
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed == "stages:" || trimmed.starts_with('#') {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("- ") {
                close(&mut name, &mut run, &mut gate, &mut stages)?;
                if let Some(found) = value(rest, "name") {
                    name = Some(found);
                    continue;
                }
                if let Some(found) = value(rest, "run") {
                    run = Some(found);
                    continue;
                }
                return Err(refused("holds an unknown stage field"));
            }
            if let Some(found) = value(trimmed, "name") {
                name = Some(found);
                continue;
            }
            if let Some(found) = value(trimmed, "run") {
                run = Some(found);
                continue;
            }
            if let Some(found) = value(trimmed, "gate") {
                gate = Some(
                    found
                        .parse::<f64>()
                        .map_err(|_| refused("holds a gate that is not a percentage"))?,
                );
                continue;
            }
            return Err(refused("holds a field it does not know"));
        }
        close(&mut name, &mut run, &mut gate, &mut stages)?;
        match stages.is_empty() {
            true => Err(refused("holds no stage")),
            false => Ok(Pipeline { stages }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_definition_reads_back_in_order() {
        let held = Pipeline::parse(
            "stages:\n  - name: one\n    run: true\n  - name: two\n    run: false\n    gate: 85\n",
        )
        .expect("the definition parses");
        assert_eq!(held.stages.len(), 2);
        assert_eq!(held.stages[0].name, "one");
        assert_eq!(held.stages[0].gate, None);
        assert_eq!(held.stages[1].gate, Some(85.0));
    }

    #[test]
    fn an_unknown_field_or_a_broken_gate_is_refused() {
        assert!(
            Pipeline::parse("stages:\n  - name: one\n    run: true\n    when: never\n").is_err()
        );
        assert!(
            Pipeline::parse("stages:\n  - name: one\n    run: true\n    gate: soon\n").is_err()
        );
        assert!(Pipeline::parse("stages:\n  - when: never\n").is_err());
    }
}
