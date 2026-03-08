use fhir_core::{Error, FhirVersion};
use fhir_tools::catalogue::{catalogue, Content};
use std::path::Path;

const USAGE: &str = "usage: catalogue <artifacts> <package> <into> <version>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    match run(&args[0], &args[1], &args[2], &args[3]) {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn run(artifacts: &str, package: &str, into: &str, named: &str) -> Result<String, Error> {
    let version = named.parse::<FhirVersion>()?;
    std::fs::create_dir_all(into).map_err(|reason| Error::Config(reason.to_string()))?;
    let content = Content::read(Path::new(artifacts), Path::new(package), version)?;
    let text = catalogue(version, &content)?;
    let path = Path::new(into).join(format!("{}.json", version.as_str().to_ascii_lowercase()));
    std::fs::write(&path, text.as_bytes())
        .map_err(|reason| Error::Config(format!("{}: {reason}", path.display())))?;
    Ok(format!("{} {}", version.as_str(), text.len()))
}
