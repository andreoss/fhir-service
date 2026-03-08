use fhir_core::{Error, FhirVersion};
use fhir_tools::models::{definitions, Artifacts};
use std::path::Path;

const USAGE: &str = "usage: models <artifacts> <definitions> [version ...]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    match run(&args[0], &args[1], &args[2..]) {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn run(artifacts: &str, into: &str, named: &[String]) -> Result<String, Error> {
    let wanted: Vec<FhirVersion> = match named.is_empty() {
        true => FhirVersion::ALL.to_vec(),
        false => named
            .iter()
            .map(|name| name.parse::<FhirVersion>())
            .collect::<Result<Vec<_>, Error>>()?,
    };
    std::fs::create_dir_all(into).map_err(|reason| Error::Config(reason.to_string()))?;
    let mut written = Vec::new();
    for version in wanted {
        let read = Artifacts::read(Path::new(artifacts), version)?;
        let text = definitions(version, &read)?;
        let path = Path::new(into).join(format!("{}.json", version.as_str().to_ascii_lowercase()));
        std::fs::write(&path, text.as_bytes())
            .map_err(|reason| Error::Config(format!("{}: {reason}", path.display())))?;
        written.push(format!("{} {}", version.as_str(), text.len()));
    }
    Ok(written.join("\n"))
}
