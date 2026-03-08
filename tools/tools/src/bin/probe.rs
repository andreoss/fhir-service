use fhir_core::Error;
use fhir_tools::http;

const USAGE: &str = "usage: probe <address> [path]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(address) = args.first() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let path = args.get(1).map(String::as_str).unwrap_or(http::HEALTH);
    match run(address, path) {
        Ok(status) => println!("{status}"),
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

fn run(address: &str, path: &str) -> Result<u16, Error> {
    let status = http::status(address, path)?;
    match status {
        200 => Ok(status),
        other => Err(Error::Internal(format!("the address answered {other}"))),
    }
}
