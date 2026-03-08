use fhir_core::Error;
use fhir_tools::{migrate, Session};

const USAGE: &str = "usage: migrate pull|reconcile <source-base>";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(command), Some(base)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    match run(command, base).await {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

async fn run(command: &str, base: &str) -> Result<String, Error> {
    let config = fhir_host::Config::from_env()?;
    let session = Session::open(&config).await?;
    let store = session.store();
    let value = match command {
        "pull" => migrate::pull(store.as_ref(), base, session.version())
            .await?
            .to_value(),
        "reconcile" => migrate::reconcile(store.as_ref(), base, session.version())
            .await?
            .to_value(),
        _ => {
            return Err(Error::Config(format!(
                "unknown migration step {command:?}; {USAGE}"
            )))
        }
    };
    Ok(value.to_string())
}
