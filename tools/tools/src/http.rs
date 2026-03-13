use fhir_core::Error;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

pub const HEALTH: &str = "/health";

pub const ENV_HEADER: &str = "FHIR_TOOL_HEADER";

fn extra() -> String {
    match std::env::var(ENV_HEADER) {
        Ok(held) if held.contains(':') => format!("{}\r\n", held.trim()),
        _ => String::new(),
    }
}

pub fn status(address: &str, path: &str) -> Result<u16, Error> {
    let stream = TcpStream::connect(address)
        .map_err(|error| Error::Internal(format!("the address refused a connection: {error}")))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| Error::Internal(format!("the read timeout is refused: {error}")))?;
    let mut stream = stream;
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n{}\r\n",
        extra()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| Error::Internal(format!("the request is not written: {error}")))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| Error::Internal(format!("the response is not read: {error}")))?;
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| Error::Internal(format!("the address answered {response:?}")))
}

pub type Reply = (u16, Vec<(String, String)>, String);

pub fn send_typed(
    address: &str,
    host: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: &str,
) -> Result<Reply, Error> {
    let stream = TcpStream::connect(address)
        .map_err(|error| Error::Internal(format!("the address refused a connection: {error}")))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| Error::Internal(format!("the read timeout is refused: {error}")))?;
    let mut stream = stream;
    let mut request = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\nAccept: application/fhir+json\r\n"
    );
    request.push_str(&extra());
    if !body.is_empty() {
        request.push_str(&format!("Content-Type: {content_type}\r\n"));
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .map_err(|error| Error::Internal(format!("the request is not written: {error}")))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| Error::Internal(format!("the response is not read: {error}")))?;
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| Error::Internal(format!("the address answered {response:?}")))?;
    let (head, held) = response
        .split_once("\r\n\r\n")
        .map(|(head, body)| (head.to_owned(), body.to_owned()))
        .unwrap_or_else(|| (response.clone(), String::new()));
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
        .collect();
    Ok((status, headers, held))
}

pub fn send(
    address: &str,
    host: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(u16, String), Error> {
    let stream = TcpStream::connect(address)
        .map_err(|error| Error::Internal(format!("the address refused a connection: {error}")))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| Error::Internal(format!("the read timeout is refused: {error}")))?;
    let mut stream = stream;
    let mut request = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\nAccept: application/fhir+json\r\n"
    );
    request.push_str(&extra());
    if !body.is_empty() {
        request.push_str("Content-Type: application/fhir+json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .map_err(|error| Error::Internal(format!("the request is not written: {error}")))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| Error::Internal(format!("the response is not read: {error}")))?;
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| Error::Internal(format!("the address answered {response:?}")))?;
    let held = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    Ok((status, held))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_address_is_reported_rather_than_waited_on() {
        assert!(status("127.0.0.1:1", HEALTH).is_err());
        assert!(send("127.0.0.1:1", "localhost", "GET", HEALTH, "").is_err());
    }
}
