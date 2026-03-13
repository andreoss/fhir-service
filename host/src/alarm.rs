use fhir_core::Error;
use fhir_telemetry::Alarm;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Called {
    address: String,
    path: String,
}

impl Called {
    pub fn parse(raw: &str) -> Result<Called, Error> {
        let held = raw.trim().trim_start_matches("http://");
        if held.starts_with("https://") {
            return Err(Error::Config(
                "the alert address is called over plain HTTP; put a proxy in front of a TLS \
                 endpoint rather than naming one here"
                    .to_owned(),
            ));
        }
        let (address, path) = match held.split_once('/') {
            None => (held, "/".to_owned()),
            Some((address, path)) => (address, format!("/{path}")),
        };
        if !address.contains(':') {
            return Err(Error::Config(format!(
                "the alert address {raw:?} names no port"
            )));
        }
        Ok(Called {
            address: address.to_owned(),
            path,
        })
    }

    fn post(&self, line: &str) -> Result<(), String> {
        let body = serde_json::json!({"alert": line}).to_string();
        let stream = TcpStream::connect(&self.address).map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(PATIENCE))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(PATIENCE))
            .map_err(|error| error.to_string())?;
        let mut stream = stream;
        let request = format!(
            "POST {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.path,
            self.address,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut answered = String::new();
        stream
            .read_to_string(&mut answered)
            .map_err(|error| error.to_string())?;
        let status = answered
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| format!("the address answered {answered:?}"))?;
        match (200..400).contains(&status) {
            true => Ok(()),
            false => Err(format!("the address answered {status}")),
        }
    }
}

impl Alarm for Called {
    fn raise(&self, line: &str) -> Result<(), String> {
        self.post(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_read_with_or_without_a_path() {
        let bare = Called::parse("127.0.0.1:9000").expect("an address");
        assert_eq!(bare.path, "/");
        let pathed = Called::parse("http://127.0.0.1:9000/alerts").expect("an address");
        assert_eq!(pathed.address, "127.0.0.1:9000");
        assert_eq!(pathed.path, "/alerts");
    }

    #[test]
    fn what_is_no_address_fails_fast() {
        assert!(Called::parse("nowhere").is_err());
        assert!(Called::parse("https://pager.example.org/x").is_err());
    }

    #[test]
    fn an_address_nobody_answers_is_reported_rather_than_waited_on() {
        let held = Called::parse("127.0.0.1:1").expect("an address");
        assert!(held
            .raise("operation=read outcome=server_fault duration_ms=1")
            .is_err());
    }
}
