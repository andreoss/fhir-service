use fhir_core::Error;
use fhir_tools::http;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

const USAGE: &str = "usage: instances <count> <service binary>";

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
    let mut running: Vec<(Child, String)> = Vec::new();
    let mut failure = None;
    for _ in 0..count {
        match start(&args[1]) {
            Ok(started) => running.push(started),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let mut reported = Vec::new();
    if failure.is_none() {
        for (_, address) in &running {
            match http::status(address, http::HEALTH) {
                Ok(status) => reported.push(json!({ "address": address, "status": status })),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
    }
    for (child, _) in &mut running {
        let _ = child.kill();
        let _ = child.wait();
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(json!({ "instances": reported }).to_string()),
    }
}

fn start(binary: &str) -> Result<(Child, String), Error> {
    let mut child = Command::new(binary)
        .env("FHIR_BIND", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| Error::Config(format!("the service cannot start: {error}")))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::Internal("the service said nothing".to_owned()));
    };
    let mut line = String::new();
    let read = BufReader::new(stdout).read_line(&mut line);
    let announced = line
        .trim()
        .rsplit_once(' ')
        .map(|(_, address)| address.to_owned());
    match (read, announced) {
        (Ok(_), Some(address)) if !address.is_empty() => Ok((child, address)),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            Err(Error::Internal(
                "the service announced no address".to_owned(),
            ))
        }
    }
}
