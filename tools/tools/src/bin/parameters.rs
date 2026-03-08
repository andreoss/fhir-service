use fhir_core::{Error, FhirVersion};
use fhir_tools::parameters;
use std::path::Path;

const USAGE: &str = "usage: parameters <artifacts> <into-directory>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    match run(&args) {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<String, Error> {
    let artifacts = Path::new(&args[0]);
    let into = Path::new(&args[1]);
    let mut lines = Vec::new();
    for version in FhirVersion::ALL {
        let (held, tally) = parameters::read(artifacts, version)?;
        let document = parameters::document(version, &held, &tally);
        let text = serde_json::to_string_pretty(&document)
            .map_err(|error| Error::Internal(error.to_string()))?;
        let path = into.join(format!("{}.json", version.as_str().to_lowercase()));
        std::fs::write(&path, format!("{text}\n")).map_err(|error| {
            Error::Internal(format!("{} cannot be written: {error}", path.display()))
        })?;
        let types = document["types"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        lines.push(format!(
            "{version}: {} of {} published parameters over {types} types \
             ({} composite, {} fhirpath-only, {} unexpressed)",
            tally.converted,
            tally.published,
            tally.composite,
            tally.fhirpath.len(),
            tally.unexpressed
        ));
    }
    Ok(lines.join("\n"))
}
