use fhir_adapter_relational::migration::{Migrator, State};
use fhir_adapter_relational::{Namespace, DEFAULT_URL, ENV_NAMESPACE, ENV_URL};
use sqlx::postgres::PgPoolOptions;

const USAGE: &str = "usage: apply version | next | latest | force <version> | unattended";
const REFUSED: i32 = 3;

enum Command {
    Version,
    Next,
    Latest,
    Force(u32),
    Unattended,
}

struct Refused(String);

fn command(args: &[String]) -> Option<Command> {
    match args.first().map(String::as_str) {
        Some("version") => Some(Command::Version),
        Some("next") => Some(Command::Next),
        Some("latest") => Some(Command::Latest),
        Some("force") => args
            .get(1)
            .and_then(|raw| raw.parse().ok())
            .map(Command::Force),
        Some("unattended") => Some(Command::Unattended),
        _ => None,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = command(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    match run(command).await {
        Ok(None) => {}
        Ok(Some(Refused(reason))) => {
            eprintln!("{reason}");
            std::process::exit(REFUSED);
        }
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

async fn run(command: Command) -> Result<Option<Refused>, String> {
    let raw =
        std::env::var(ENV_NAMESPACE).unwrap_or_else(|_| Namespace::default().as_str().to_owned());
    let namespace = Namespace::parse(&raw).map_err(|error| error.to_string())?;
    let url = std::env::var(ENV_URL).unwrap_or_else(|_| DEFAULT_URL.to_owned());
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&url)
        .await
        .map_err(|error| format!("the engine is not available: {error}"))?;
    let migrator = Migrator::new(pool, namespace);
    match command {
        Command::Version => {
            let report = migrator
                .compatibility()
                .await
                .map_err(|error| error.to_string())?;
            let current = report
                .current
                .map(|version| version.to_string())
                .unwrap_or_else(|| "none".to_owned());
            println!(
                "schema {current} instance {} lowest {} {}",
                report.instance,
                report.lowest,
                report.state.as_str()
            );
        }
        Command::Next => match migrator.next().await.map_err(|error| error.to_string())? {
            Some(version) => println!("applied {version}"),
            None => println!("no pending version"),
        },
        Command::Latest => {
            let applied = migrator.latest().await.map_err(|error| error.to_string())?;
            println!("applied {applied}");
        }
        Command::Force(version) => {
            migrator
                .force(version)
                .await
                .map_err(|error| error.to_string())?;
            println!("forced {version}");
        }
        Command::Unattended => {
            let report = migrator
                .compatibility()
                .await
                .map_err(|error| error.to_string())?;
            if report.state == State::Ahead {
                let found = report.current.unwrap_or_default();
                return Ok(Some(Refused(format!(
                    "schema {found} is ahead of this build at {}",
                    report.instance
                ))));
            }
            let applied = migrator.latest().await.map_err(|error| error.to_string())?;
            match applied {
                0 => println!("current {}", report.instance),
                count => println!("applied {count} to {}", report.instance),
            }
        }
    }
    Ok(None)
}
