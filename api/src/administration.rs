use axum::extract::{ConnectInfo, Request, State};
use axum::http::{StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fhir_core::{Error, IssueCode, OperationOutcome};
use std::net::{IpAddr, SocketAddr};

pub const DOOR: &str = "/administration";

const TYPES: &[&str] = &[
    "SearchParameter",
    "StructureDefinition",
    "CompartmentDefinition",
    "ValueSet",
    "CodeSystem",
    "ConceptMap",
    "OperationDefinition",
    "AccessPolicy",
];

const OPERATIONS: &[&str] = &["$reindex", "$refresh", "$preload", "$reset"];

const CLINICAL: &[&str] = &[
    "$expand",
    "$validate-code",
    "$lookup",
    "$translate",
    "$subsumes",
    "$closure",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    at: IpAddr,
    bits: u8,
}

impl Network {
    pub fn parse(text: &str) -> Result<Network, Error> {
        let text = text.trim();
        let (address, bits) = match text.split_once('/') {
            Some((address, bits)) => {
                let bits = bits.parse::<u8>().map_err(|_| {
                    Error::Config(format!(
                        "{text:?} does not name a network: {bits:?} is no width"
                    ))
                })?;
                (address, Some(bits))
            }
            None => (text, None),
        };
        let at = address
            .parse::<IpAddr>()
            .map_err(|_| Error::Config(format!("{text:?} does not name a network")))?;
        let whole = match at {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let bits = bits.unwrap_or(whole);
        if bits > whole {
            return Err(Error::Config(format!(
                "{text:?} does not name a network: {bits} bits of {whole}"
            )));
        }
        Ok(Network { at, bits })
    }

    pub fn holds(&self, address: IpAddr) -> bool {
        let address = unmapped(address);
        match (self.at, address) {
            (IpAddr::V4(at), IpAddr::V4(address)) => {
                leading(&at.octets(), &address.octets(), self.bits)
            }
            (IpAddr::V6(at), IpAddr::V6(address)) => {
                leading(&at.octets(), &address.octets(), self.bits)
            }
            _ => false,
        }
    }
}

fn unmapped(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(held) => match held.to_ipv4_mapped() {
            Some(held) => IpAddr::V4(held),
            None => IpAddr::V6(held),
        },
        held => held,
    }
}

fn leading(one: &[u8], other: &[u8], bits: u8) -> bool {
    let whole = (bits / 8) as usize;
    if one[..whole] != other[..whole] {
        return false;
    }
    let left = bits % 8;
    if left == 0 {
        return true;
    }
    let mask = 0xffu8 << (8 - left);
    one[whole] & mask == other[whole] & mask
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Administration {
    networks: Option<Vec<Network>>,
}

impl Administration {
    pub fn off() -> Administration {
        Administration::default()
    }

    pub fn restricted_to<I, S>(networks: I) -> Result<Administration, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let held = networks
            .into_iter()
            .map(|network| Network::parse(network.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        if held.is_empty() {
            return Err(Error::Config(
                "an administration door admitting no network would serve nobody".to_owned(),
            ));
        }
        Ok(Administration {
            networks: Some(held),
        })
    }

    pub fn is_on(&self) -> bool {
        self.networks.is_some()
    }

    pub fn admits(&self, address: IpAddr) -> bool {
        match &self.networks {
            None => true,
            Some(networks) => networks.iter().any(|network| network.holds(address)),
        }
    }
}

pub fn administrative(path: &str) -> bool {
    let path = path.trim_start_matches('/');
    let mut parts = path.split('/');
    let Some(first) = parts.next() else {
        return false;
    };
    if CLINICAL.iter().any(|held| path.contains(held)) {
        return false;
    }
    if OPERATIONS.contains(&first) {
        return true;
    }
    if path
        .rsplit('/')
        .next()
        .is_some_and(|last| OPERATIONS.contains(&last))
    {
        return true;
    }
    TYPES.contains(&first)
}

pub async fn doors(
    State(state): State<crate::app::AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let address = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(held)| *held);
    let path = request.uri().path().to_owned();
    let Some(behind) = path.strip_prefix(DOOR) else {
        return match administrative(&path) {
            true => missing(&path),
            false => next.run(request).await,
        };
    };
    let Some(address) = address else {
        return refused(None);
    };
    if !state.administration.admits(address.ip()) {
        return refused(Some(address.ip()));
    }
    let behind = match behind.is_empty() {
        true => "/".to_owned(),
        false => behind.to_owned(),
    };
    let rest = match request.uri().query() {
        Some(query) => format!("{behind}?{query}"),
        None => behind,
    };
    match rest.parse::<Uri>() {
        Ok(uri) => *request.uri_mut() = uri,
        Err(_) => return missing(&path),
    }
    next.run(request).await
}

fn missing(path: &str) -> Response {
    let outcome = OperationOutcome::error(
        IssueCode::NotFound,
        format!("{path} is not served on this door"),
    );
    answer(StatusCode::NOT_FOUND, outcome)
}

fn refused(address: Option<IpAddr>) -> Response {
    let said = match address {
        Some(address) => format!("{address} is not on a network this door admits"),
        None => "this door admits a named network, and this request carries no address".to_owned(),
    };
    let outcome = OperationOutcome::error(IssueCode::Forbidden, said);
    answer(StatusCode::FORBIDDEN, outcome)
}

fn answer(status: StatusCode, outcome: OperationOutcome) -> Response {
    let mut response = (status, outcome.to_fhir_json()).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/fhir+json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> IpAddr {
        text.parse().expect("an address parses")
    }

    #[test]
    fn a_bare_address_is_the_network_of_itself() {
        let network = Network::parse("10.0.0.1").expect("an address is a network");
        assert!(network.holds(address("10.0.0.1")));
        assert!(!network.holds(address("10.0.0.2")));
    }

    #[test]
    fn a_width_that_is_not_a_whole_byte_is_still_a_width() {
        let network = Network::parse("10.0.0.0/12").expect("twelve bits are a width");
        assert!(network.holds(address("10.15.255.255")));
        assert!(!network.holds(address("10.16.0.0")));
    }

    #[test]
    fn a_v4_address_written_as_a_v6_one_is_the_v4_address() {
        let network = Network::parse("127.0.0.0/8").expect("a network parses");
        assert!(
            network.holds(address("::ffff:127.0.0.1")),
            "a dual-stack listener reports a v4 peer this way, and the operator \
             who named 127.0.0.0/8 meant it"
        );
    }

    #[test]
    fn the_two_families_do_not_hold_each_other() {
        let network = Network::parse("::1/128").expect("a network parses");
        assert!(network.holds(address("::1")));
        assert!(!network.holds(address("127.0.0.1")));
    }

    #[test]
    fn what_is_no_network_is_refused() {
        assert!(Network::parse("10.0.0.0/33").is_err());
        assert!(Network::parse("10.0.0.0/wide").is_err());
        assert!(Network::parse("nowhere").is_err());
    }

    #[test]
    fn a_shut_door_admits_everyone_because_there_is_only_one() {
        let administration = Administration::off();
        assert!(!administration.is_on());
        assert!(administration.admits(address("9.9.9.9")));
    }

    #[test]
    fn what_belongs_to_the_other_door() {
        assert!(administrative("/SearchParameter"));
        assert!(administrative("/StructureDefinition/1"));
        assert!(administrative("/ValueSet"));
        assert!(administrative("/$reindex"));
        assert!(administrative("/Patient/1/$reindex"));
        assert!(!administrative("/Patient"));
        assert!(!administrative("/metadata"));
        assert!(!administrative("/ValueSet/$expand"));
        assert!(
            !administrative("/CodeSystem/$lookup"),
            "a terminology question is asked of an administrative type but is \
             not an administrative question"
        );
    }
}
