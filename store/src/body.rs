use fhir_core::Error;
use std::io::{Read, Write};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Plain,
    Packed,
}

impl Encoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Encoding::Plain => "plain",
            Encoding::Packed => "packed",
        }
    }

    pub fn parse(raw: &str) -> Result<Encoding, Error> {
        match raw {
            "plain" => Ok(Encoding::Plain),
            "packed" => Ok(Encoding::Packed),
            other => Err(Error::Internal(format!("unknown body encoding {other:?}"))),
        }
    }
}

pub fn encoded(raw: &[u8]) -> (Vec<u8>, Encoding) {
    let mut packer = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    let packed = packer
        .write_all(raw)
        .and_then(|()| packer.finish())
        .ok()
        .filter(|packed| packed.len() < raw.len());
    match packed {
        Some(packed) => (packed, Encoding::Packed),
        None => (raw.to_vec(), Encoding::Plain),
    }
}

pub fn decoded(stored: &[u8], encoding: Encoding) -> Result<Vec<u8>, Error> {
    match encoding {
        Encoding::Plain => Ok(stored.to_vec()),
        Encoding::Packed => {
            let mut unpacked = Vec::new();
            flate2::read::DeflateDecoder::new(stored)
                .read_to_end(&mut unpacked)
                .map_err(|error| Error::Internal(format!("stored body is unreadable: {error}")))?;
            Ok(unpacked)
        }
    }
}

#[derive(Debug)]
pub struct LazyBody {
    stored: Vec<u8>,
    encoding: Encoding,
    unpacked: OnceLock<Vec<u8>>,
}

impl LazyBody {
    pub fn new(stored: Vec<u8>, encoding: Encoding) -> LazyBody {
        LazyBody {
            stored,
            encoding,
            unpacked: OnceLock::new(),
        }
    }

    pub fn bytes(&self) -> Result<&[u8], Error> {
        if let Some(found) = self.unpacked.get() {
            return Ok(found);
        }
        let unpacked = decoded(&self.stored, self.encoding)?;
        let _ = self.unpacked.set(unpacked);
        Ok(self.unpacked.get().map(Vec::as_slice).unwrap_or_default())
    }

    pub fn stored_len(&self) -> usize {
        self.stored.len()
    }

    pub fn is_unpacked(&self) -> bool {
        self.unpacked.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(raw: &[u8]) {
        let (stored, encoding) = encoded(raw);
        assert_eq!(decoded(&stored, encoding).unwrap(), raw);
    }

    #[test]
    fn packing_a_body_loses_nothing() {
        round_trip(b"");
        round_trip(b"{}");
        round_trip("{\"n\":\"\u{5f20}\u{4e09}\",\"e\":\"\u{1f600}\"}".as_bytes());
        round_trip(&(0..=255u8).collect::<Vec<u8>>());
        round_trip(r#"{"a":[1,2,3],"b":{"c":null,"d":true}}"#.as_bytes());
    }

    #[test]
    fn a_repetitive_body_is_held_smaller_than_it_was_written() {
        let raw = format!(r#"{{"note":"{}"}}"#, "repeat ".repeat(200)).into_bytes();
        let (stored, encoding) = encoded(&raw);
        assert_eq!(encoding, Encoding::Packed);
        assert!(
            stored.len() < raw.len() / 4,
            "{} vs {}",
            stored.len(),
            raw.len()
        );
        assert_eq!(decoded(&stored, encoding).unwrap(), raw);
    }

    #[test]
    fn a_body_that_would_grow_is_held_as_written() {
        let (stored, encoding) = encoded(b"{}");
        assert_eq!(encoding, Encoding::Plain);
        assert_eq!(stored, b"{}");
    }

    #[test]
    fn an_unknown_encoding_is_refused() {
        assert_eq!(Encoding::parse("plain").unwrap(), Encoding::Plain);
        assert_eq!(Encoding::parse("packed").unwrap(), Encoding::Packed);
        assert!(Encoding::parse("sideways").is_err());
        assert_eq!(Encoding::Packed.as_str(), "packed");
    }

    #[test]
    fn a_lazy_body_unpacks_once_and_only_when_asked() {
        let raw = format!(r#"{{"note":"{}"}}"#, "repeat ".repeat(200)).into_bytes();
        let (stored, encoding) = encoded(&raw);
        let body = LazyBody::new(stored, encoding);
        assert!(!body.is_unpacked());
        assert!(body.stored_len() < raw.len());
        assert_eq!(body.bytes().unwrap(), raw);
        assert!(body.is_unpacked());
        assert_eq!(body.bytes().unwrap(), raw);
    }

    #[test]
    fn a_damaged_packed_body_is_reported_rather_than_returned() {
        let body = LazyBody::new(vec![9, 9, 9, 9], Encoding::Packed);
        assert!(body.bytes().is_err());
    }
}
