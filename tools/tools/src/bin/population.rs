use fhir_core::{Error, FhirVersion, Model};
use fhir_tools::population;

const USAGE: &str = "usage: population <count> <file> [version] [seed]";

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
    let count = args[0]
        .parse::<usize>()
        .map_err(|_| Error::InvalidParameter(format!("count {:?}", args[0])))?;
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
    let supply = population::population(version, count, seed)?;
    let model = Model::of(version);
    let mut rows = 0usize;
    for line in supply.lines() {
        let body: serde_json::Value =
            serde_json::from_str(line).map_err(|e| Error::InvalidJson(e.to_string()))?;
        let findings = model.check(&body);
        if !findings.is_empty() {
            return Err(Error::Internal(format!(
                "the generator emitted a body {version} refuses: {findings:?}"
            )));
        }
        rows += 1;
    }
    std::fs::write(&args[1], &supply)
        .map_err(|error| Error::Internal(format!("the supply cannot be written: {error}")))?;
    Ok(format!(
        "generated {count} subjects as {rows} resources for {version} seed {seed}; \
         every body checked against the definitions; codes from {} declared unverified",
        population::UNVERIFIED_SYSTEMS.join(", ")
    ))
}
