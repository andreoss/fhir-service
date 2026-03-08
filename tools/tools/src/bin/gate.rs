use fhir_core::Error;
use fhir_tools::coverage;

const USAGE: &str = "usage: gate <coverage report> [percentage]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    match run(path, args.get(1).map(String::as_str)) {
        Ok(report) => println!("{report}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn run(path: &str, wanted: Option<&str>) -> Result<String, Error> {
    let required = match wanted {
        Some(raw) => raw
            .parse::<f64>()
            .map_err(|_| Error::InvalidParameter(format!("percentage {raw:?}")))?,
        None => coverage::REQUIRED,
    };
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::Config(format!("the coverage report cannot be read: {error}")))?;
    let percent = coverage::lines_percent(&text)?;
    match coverage::clears(percent, required) {
        true => Ok(format!("lines {percent:.2} of {required} required")),
        false => Err(Error::Internal(format!(
            "lines {percent:.2} is under the {required} required"
        ))),
    }
}
