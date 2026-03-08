use fhir_core::{Error, FhirVersion};
use fhir_tools::population::history;
use fhir_tools::{upload, verify};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: census <address> <subjects> <version> <seed> <backend> [into.json]\n\
     loads a population, checks it, and writes the figures.";
const PATIENCE: Duration = Duration::from_secs(300);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
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
    let address = &args[0];
    let subjects = args[1]
        .parse::<u64>()
        .map_err(|_| Error::InvalidParameter(format!("subjects {:?}", args[1])))?;
    let version = args[2].parse::<FhirVersion>()?;
    let seed = args[3]
        .parse::<u64>()
        .map_err(|_| Error::InvalidParameter(format!("seed {:?}", args[3])))?;
    let backend = args[4].clone();

    let records: Vec<_> = (0..subjects)
        .flat_map(|position| history(version, seed, position))
        .collect();
    let versions: usize = records.iter().map(|record| record.changes.len()).sum();

    let began = Instant::now();
    let loaded = upload::load(address, "localhost", &records, PATIENCE)?;
    let load_seconds = began.elapsed().as_secs_f64();

    let began = Instant::now();
    let predicted = verify::predicted(&records);
    let observed = verify::observed(address, "localhost", predicted.keys().cloned())?;
    let differences = verify::differences(&predicted, &observed);
    let refused = verify::validated(address, "localhost", &records)?;
    let check_seconds = began.elapsed().as_secs_f64();

    let figures = verify::Figures {
        version,
        backend,
        subjects,
        seed,
        records: records.len(),
        versions,
        by_type: predicted,
        load_seconds,
        check_seconds,
        refused: refused.len(),
        differences: differences.clone(),
    };
    if let Some(into) = args.get(5) {
        let text = serde_json::to_string_pretty(&figures.to_json())
            .map_err(|error| Error::Internal(error.to_string()))?;
        std::fs::write(into, format!("{text}\n"))
            .map_err(|error| Error::Internal(format!("the figures cannot be written: {error}")))?;
    }
    if !refused.is_empty() || !differences.is_empty() {
        return Err(Error::Internal(format!(
            "{} refused, {} counts disagree: {refused:?} {differences:?}",
            refused.len(),
            differences.len()
        )));
    }
    Ok(format!(
        "{version} on {} : {} records, {versions} versions, {} written, {} settled; \
         load {load_seconds:.2}s, check {check_seconds:.2}s; 0 refused, 0 differences",
        figures.backend, figures.records, loaded.written, loaded.unchanged
    ))
}
