use fhir_core::{Error, FhirVersion};
use fhir_tools::population::history;
use fhir_tools::upload;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: upload <address> <subjects> [version] [seed] [bundle]\n\
     loads a synthetic population into a running instance through $import and,\n\
     when `bundle` is given, through transaction bundles instead.";
const PATIENCE: Duration = Duration::from_secs(300);

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
    let address = &args[0];
    let subjects = args[1]
        .parse::<u64>()
        .map_err(|_| Error::InvalidParameter(format!("subjects {:?}", args[1])))?;
    let version = match args.get(2) {
        Some(raw) => raw.parse::<FhirVersion>()?,
        None => FhirVersion::R4,
    };
    let seed = match args.get(3) {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| Error::InvalidParameter(format!("seed {raw:?}")))?,
        None => 1,
    };
    let bundles = args.get(4).is_some_and(|raw| raw == "bundle");

    let records: Vec<_> = (0..subjects)
        .flat_map(|position| history(version, seed, position))
        .collect();
    let versions: usize = records.iter().map(|record| record.changes.len()).sum();
    let started = Instant::now();

    let report = match bundles {
        true => {
            let mut sent = 0usize;
            let mut failures = 0usize;
            for bundle in upload::transactions(&records) {
                let loaded = upload::transact(address, "localhost", &bundle)?;
                sent += loaded.submitted;
                failures += loaded.failures;
            }
            format!("{sent} entries through transaction bundles, {failures} failures")
        }
        false => {
            let loaded = upload::load(address, "localhost", &records, PATIENCE)?;
            format!(
                "{} rows through $import, {} already settled, {} failures",
                loaded.written, loaded.unchanged, loaded.failures
            )
        }
    };
    Ok(format!(
        "{subjects} subjects as {} records and {versions} versions for {version} seed {seed}: \
         {report}; {:.2}s",
        records.len(),
        started.elapsed().as_secs_f64()
    ))
}
