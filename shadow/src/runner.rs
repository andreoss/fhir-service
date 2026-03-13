use crate::case::{Answer, Case, Plan};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub trait Side {
    fn label(&self) -> &str;

    fn base(&self) -> &str {
        ""
    }

    fn send(&self, case: &Case) -> Result<Answer, String>;
}

pub struct SideRun {
    pub label: String,

    pub base: String,

    pub answers: Vec<Result<Answer, String>>,
}

pub struct Run {
    pub left: SideRun,

    pub right: SideRun,
}

pub struct Targets {
    left: (String, u16),
    right: (String, u16),
}

impl Targets {
    pub fn of(left: (&str, u16), right: (&str, u16)) -> Result<Targets, String> {
        if left.0 == right.0 && left.1 == right.1 {
            return Err("both sides name one address, so neither is independent".to_owned());
        }
        Ok(Targets {
            left: (left.0.to_owned(), left.1),
            right: (right.0.to_owned(), right.1),
        })
    }

    pub fn left(&self) -> (&str, u16) {
        (&self.left.0, self.left.1)
    }

    pub fn right(&self) -> (&str, u16) {
        (&self.right.0, self.right.1)
    }
}

pub fn address(text: &str) -> Option<(String, u16, String)> {
    let (authority, prefix) = match text.find('/') {
        Some(slash) => (
            &text[..slash],
            text[slash..].trim_end_matches('/').to_owned(),
        ),
        None => (text, String::new()),
    };
    let (host, port) = authority.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?, prefix))
}

pub struct HttpSide {
    label: String,
    host: String,
    port: u16,
    prefix: String,
    headers: Vec<(String, String)>,
    patience: Duration,
}

impl HttpSide {
    pub fn of(label: &str, host: &str, port: u16) -> HttpSide {
        HttpSide {
            label: label.to_owned(),
            host: host.to_owned(),
            port,
            prefix: String::new(),
            headers: Vec::new(),
            patience: Duration::from_secs(30),
        }
    }

    pub fn under(mut self, prefix: &str) -> HttpSide {
        self.prefix = prefix.trim_end_matches('/').to_owned();
        self
    }

    pub fn carrying(mut self, name: &str, value: &str) -> HttpSide {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn waiting(mut self, patience: Duration) -> HttpSide {
        self.patience = patience;
        self
    }
}

impl Side for HttpSide {
    fn label(&self) -> &str {
        &self.label
    }

    fn base(&self) -> &str {
        &self.prefix
    }

    fn send(&self, case: &Case) -> Result<Answer, String> {
        let stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|error| format!("no connection: {error}"))?;
        stream
            .set_read_timeout(Some(self.patience))
            .map_err(|error| format!("no read bound: {error}"))?;
        stream
            .set_write_timeout(Some(self.patience))
            .map_err(|error| format!("no write bound: {error}"))?;
        let mut stream = stream;
        let target = format!("{}{}", self.prefix, case.path);
        let mut head = format!(
            "{} {} HTTP/1.0\r\nHost: {}:{}\r\nConnection: close\r\nAccept: application/fhir+json\r\n",
            case.method, target, self.host, self.port
        );
        for (name, value) in self.headers.iter().chain(case.headers.iter()) {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if !case.body.is_empty() {
            head.push_str(&format!("Content-Length: {}\r\n", case.body.len()));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(case.body.as_bytes());
        stream
            .write_all(&bytes)
            .map_err(|error| format!("the request was not sent: {error}"))?;
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|error| format!("no answer was read: {error}"))?;
        parse(&String::from_utf8_lossy(&raw))
    }
}

fn parse(text: &str) -> Result<Answer, String> {
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default().to_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("no response code in {status_line:?}"))?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Answer {
        status,
        headers,
        body,
    })
}

fn resolve(text: &str, plan: &Plan, answers: &[Result<Answer, String>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{etag:") {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 6..];
        match tail.find('}') {
            Some(close) => {
                let name = &tail[..close];
                let found = plan
                    .cases()
                    .iter()
                    .position(|case| case.name == name)
                    .and_then(|index| answers.get(index))
                    .and_then(|answer| answer.as_ref().ok())
                    .and_then(|answer| answer.header("etag"))
                    .unwrap_or_default();
                out.push_str(found);
                rest = &tail[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn drive(plan: &Plan, side: &dyn Side) -> SideRun {
    let mut answers: Vec<Result<Answer, String>> = Vec::with_capacity(plan.cases().len());
    for case in plan.cases() {
        let mut sending = case.clone();
        sending.headers = case
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), resolve(value, plan, &answers)))
            .collect();
        sending.body = resolve(&case.body, plan, &answers);
        answers.push(side.send(&sending));
    }
    SideRun {
        label: side.label().to_owned(),
        base: side.base().to_owned(),
        answers,
    }
}

pub fn shadow(plan: &Plan, left: &dyn Side, right: &dyn Side) -> Result<Run, String> {
    if left.label() == right.label() {
        return Err("both sides carry one name, so the report cannot tell them apart".to_owned());
    }
    Ok(Run {
        left: drive(plan, left),
        right: drive(plan, right),
    })
}
