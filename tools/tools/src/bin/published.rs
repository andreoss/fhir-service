




use fhir_core::FhirVersion;
use fhir_tools::{http, published};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(600);

fn main() {
    let asked: Vec<String> = std::env::args().skip(1).collect();
    if asked.len() < 2 {
        eprintln!("usage: published <address> <directory> [version]");
        std::process::exit(2);
    }
    let address = asked[0].clone();
    let directory = PathBuf::from(&asked[1]);
    let version = asked
        .get(2)
        .map(|raw| raw.parse::<FhirVersion>())
        .transpose()
        .unwrap_or(None)
        .unwrap_or(FhirVersion::R4);
    match run(&address, &directory, version) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("published: {error}");
            std::process::exit(1);
        }
    }
}

fn run(address: &str, directory: &Path, version: FhirVersion) -> Result<(), fhir_core::Error> {
    let held = published::read(directory, version)?;
    println!(
        "read {} resources of {} types from {}",
        held.total(),
        held.types().len(),
        directory.display()
    );

    let refused = published::judged(&held, version)?;
    for one in refused.iter().take(10) {
        println!("refused {} {}: {}", one.resource_type, one.id, one.issue);
    }
    if !refused.is_empty() {
        return Err(fhir_core::Error::Internal(format!(
            "{} of {} resources are refused by {version}",
            refused.len(),
            held.total()
        )));
    }
    println!("every resource passes validation for {version}");

    if http::status(address, http::HEALTH)? != 200 {
        return Err(fhir_core::Error::Internal(format!(
            "{address} is not answering"
        )));
    }

    let started = Instant::now();
    
    
    
    let through_bundles = std::env::var("FHIR_TOOL_BUNDLES").is_ok();
    let loaded = match through_bundles {
        true => published::load_through_bundles(address, "localhost", &held)?,
        false => published::load(address, "localhost", &held, PATIENCE)?,
    };
    let load_seconds = started.elapsed().as_secs_f64();
    println!(
        "loaded {} written, {} unchanged, {} failures in {load_seconds:.2}s",
        loaded.written, loaded.unchanged, loaded.failures
    );
    if loaded.failures > 0 {
        return Err(fhir_core::Error::Internal(format!(
            "{} rows were refused by the import",
            loaded.failures
        )));
    }

    let counted = published::held(address, "localhost", &held.types())?;
    let figures = published::figures(&held.counts(), &counted);
    let disagreeing: Vec<&published::Figure> =
        figures.iter().filter(|figure| !figure.agrees()).collect();
    for figure in &figures {
        println!(
            "{:<22} published {:>5}  held {:>5}{}",
            figure.resource_type.as_str(),
            figure.published,
            figure.held,
            match figure.agrees() {
                true => "",
                false => "  DISAGREES",
            }
        );
    }
    if !disagreeing.is_empty() {
        return Err(fhir_core::Error::Internal(format!(
            "{} types hold a different count from what was published",
            disagreeing.len()
        )));
    }
    println!("every type holds what the published set carried");
    Ok(())
}
