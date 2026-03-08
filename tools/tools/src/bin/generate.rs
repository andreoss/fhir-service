use fhir_core::Error;
use fhir_tools::generate;

const USAGE: &str = "usage: generate <count> <file> [resource type] [seed]";

#[tokio::main(flavor = "current_thread")]
async fn main() {
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
    let label = args.get(2).map(String::as_str).unwrap_or("Patient");
    let seed = match args.get(3) {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| Error::InvalidParameter(format!("seed {raw:?}")))?,
        None => 1,
    };
    let supply = generate::rows(count, label, seed)?;
    std::fs::write(&args[1], &supply)
        .map_err(|error| Error::Internal(format!("the supply cannot be written: {error}")))?;
    Ok(format!("generated {count} {label} seed {seed}"))
}
