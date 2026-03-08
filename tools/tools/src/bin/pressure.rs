use fhir_core::Error;
use fhir_tools::measure::{compare_records, Record};
use fhir_tools::pressure::{load, soak, Plan};
use serde_json::Value;

const USAGE: &str = concat!(
    "usage: pressure load <address> <callers> <subjects> <seed> [record]\n",
    "       pressure soak <address> <callers> <seconds> <seed> [record]\n",
    "       pressure judge <baseline> <candidate>"
);
const SLOWER: &str = "slower";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.first().map(String::as_str) {
        Some("load") if args.len() >= 5 => driven(&args, false),
        Some("soak") if args.len() >= 5 => driven(&args, true),
        Some("judge") if args.len() >= 3 => judged(&args[1], &args[2]),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    match outcome {
        Ok((report, held)) => {
            println!("{report}");
            std::process::exit(if held { 0 } else { 1 });
        }
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn driven(args: &[String], long: bool) -> Result<(String, bool), Error> {
    let label = args
        .get(5)
        .and_then(|path| path.rsplit('/').next())
        .unwrap_or(&args[0])
        .to_owned();
    let seed = args[4]
        .parse::<u64>()
        .map_err(|_| Error::InvalidParameter(format!("seed {:?}", args[4])))?;
    let plan = Plan::parse(&label, &args[2], &args[3], seed)?;
    let outcome = match long {
        true => soak(&args[1], &plan)?,
        false => load(&args[1], &plan)?,
    };
    let text = outcome.to_json();
    if let Some(path) = args.get(5) {
        std::fs::write(path, &text)
            .map_err(|error| Error::Internal(format!("the record cannot be written: {error}")))?;
    }
    Ok((text, outcome.refused == 0))
}

fn judged(left: &str, right: &str) -> Result<(String, bool), Error> {
    let left = read(left)?;
    let right = read(right)?;
    let judged = compare_records(&left, &right)?;
    let held = judged.iter().all(|one| one.verdict != SLOWER);
    let rendered: Vec<Value> = judged.iter().map(|one| one.to_value()).collect();
    Ok((Value::Array(rendered).to_string(), held))
}

fn read(path: &str) -> Result<Record, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::Config(format!("the record cannot be read: {error}")))?;
    Record::parse(&text)
}
