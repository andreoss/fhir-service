use fhir_core::{Error, ResourceEnvelope, ResourceType};
use fhir_store::SearchQuery;
use fhir_tools::generate;
use fhir_tools::measure::{compare_records, Record, Run, Summary};
use fhir_tools::Session;
use std::time::Instant;

const USAGE: &str = "usage: measure run <count> [file] | measure compare <left> <right>";
const SEED: u64 = 1;
const LABEL: &str = "Patient";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.first().map(String::as_str) {
        Some("run") if args.len() >= 2 => run(&args[1..]).await,
        Some("compare") if args.len() >= 3 => judged(&args[1], &args[2]),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    match outcome {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn judged(left: &str, right: &str) -> Result<String, Error> {
    let left = Record::parse(&read(left)?)?;
    let right = Record::parse(&read(right)?)?;
    let held: Vec<serde_json::Value> = compare_records(&left, &right)?
        .iter()
        .map(|judged| judged.to_value())
        .collect();
    Ok(serde_json::Value::Array(held).to_string())
}

fn read(path: &str) -> Result<String, Error> {
    std::fs::read_to_string(path)
        .map_err(|error| Error::Config(format!("the record cannot be read: {error}")))
}

async fn run(args: &[String]) -> Result<String, Error> {
    let count = args[0]
        .parse::<usize>()
        .map_err(|_| Error::InvalidParameter(format!("count {:?}", args[0])))?;
    let config = fhir_host::Config::from_env()?;
    let session = Session::open(&config).await?;
    let store = session.store();
    let supply = generate::rows(count, LABEL, SEED)?;

    let mut writes = Vec::new();
    let mut ids = Vec::new();
    for row in supply.lines() {
        let envelope = ResourceEnvelope::parse_supplied(session.version(), row.as_bytes())?;
        ids.push(envelope.id().clone());
        let started = Instant::now();
        match store.create(envelope.clone()).await {
            Ok(_) => {}
            Err(Error::Duplicate(_)) => {
                store.update(envelope, None).await?;
            }
            Err(error) => return Err(error),
        }
        writes.push(started.elapsed().as_micros() as u64);
    }

    let mut reads = Vec::new();
    for id in &ids {
        let started = Instant::now();
        store
            .read(&fhir_core::ResourceKey::new(
                LABEL.parse::<ResourceType>()?,
                id.clone(),
            ))
            .await?;
        reads.push(started.elapsed().as_micros() as u64);
    }

    let mut searches = Vec::new();
    let query = SearchQuery::of_type(LABEL.parse::<ResourceType>()?);
    for _ in 0..count {
        let started = Instant::now();
        store.search(&query).await?;
        searches.push(started.elapsed().as_micros() as u64);
    }

    let label = args
        .get(1)
        .and_then(|path| path.rsplit('/').next())
        .unwrap_or("run")
        .to_owned();
    let record = Record::new(vec![
        summary(&label, "write", writes),
        summary(&label, "read", reads),
        summary(&label, "search", searches),
    ]);
    let text = record.to_json();
    if let Some(path) = args.get(1) {
        std::fs::write(path, &text)
            .map_err(|error| Error::Internal(format!("the record cannot be written: {error}")))?;
    }
    Ok(text)
}

fn summary(label: &str, operation: &str, samples: Vec<u64>) -> Summary {
    Run::new(label, operation, samples).summary()
}
