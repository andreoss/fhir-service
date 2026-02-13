#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

pub fn spawn_server() -> (Child, u16) {
    spawn_with(&[])
}

pub fn spawn_with(extra: &[(&str, &str)]) -> (Child, u16) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fhir-host"));
    command
        .env("FHIR_BACKEND", "memory")
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
        .env_remove("FHIR_DATABASE_URL")
        .env_remove("FHIR_METRICS_CREDENTIAL");
    for (name, value) in extra {
        command.env(name, value);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn binary");
    let stdout = child.stdout.take().expect("missing stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("failed to read the announced address");
    let port = line
        .trim()
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok());
    match port {
        Some(port) => (child, port),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("server did not announce an address, said {line:?}");
        }
    }
}


pub fn stop(mut child: Child) {
    child.kill().expect("failed to kill server");
    child.wait().expect("failed to reap server");
}

pub fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("failed to connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("set read timeout");
    let mut stream = stream;
    let mut head = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\n");
    if !body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(body);
    stream.write_all(&bytes).expect("failed to write request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("failed to read response");
    parse_response(&response)
}

fn parse_response(text: &str) -> Reply {
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().expect("missing response head");
    let body = parts.next().unwrap_or("").to_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("missing status line");
    let status = status_line.split_whitespace().nth(1).expect("missing status code").parse().expect("bad status code");
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').expect("malformed header");
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    Reply { status, headers, body }
}
