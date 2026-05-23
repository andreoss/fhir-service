use fhir_core::Error;
use std::io::{Read, Write};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Plain,
    Packed,
    Sealed,
}

impl Encoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Encoding::Plain => "plain",
            Encoding::Packed => "packed",
            Encoding::Sealed => "sealed",
        }
    }

    pub fn parse(raw: &str) -> Result<Encoding, Error> {
        match raw {
            "plain" => Ok(Encoding::Plain),
            "packed" => Ok(Encoding::Packed),
            "sealed" => Ok(Encoding::Sealed),
            other => Err(Error::Internal(format!("unknown body encoding {other:?}"))),
        }
    }
}

pub fn encoded(raw: &[u8]) -> (Vec<u8>, Encoding) {
    let mut packer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let sealed = packer
        .write_all(raw)
        .and_then(|()| packer.finish())
        .ok()
        .filter(|sealed| sealed.len() < raw.len());
    match sealed {
        Some(sealed) => (sealed, Encoding::Sealed),
        None => (raw.to_vec(), Encoding::Plain),
    }
}

fn unread(stored: &[u8], read: u64, unpacked: &[u8]) -> Result<(), Error> {
    let whole = read as usize == stored.len();
    let carried = !unpacked.is_empty() || stored.is_empty();
    match whole && carried {
        true => Ok(()),
        false => Err(Error::Internal(
            "stored body is unreadable: it ends before the body it holds does".to_owned(),
        )),
    }
}

pub fn decoded(stored: &[u8], encoding: Encoding) -> Result<Vec<u8>, Error> {
    let damaged =
        |error: std::io::Error| Error::Internal(format!("stored body is unreadable: {error}"));
    match encoding {
        Encoding::Plain => Ok(stored.to_vec()),
        Encoding::Packed => {
            let mut unpacked = Vec::new();
            let mut reader = flate2::read::DeflateDecoder::new(stored);
            reader.read_to_end(&mut unpacked).map_err(damaged)?;
            unread(stored, reader.total_in(), &unpacked)?;
            Ok(unpacked)
        }
        Encoding::Sealed => {
            let mut unpacked = Vec::new();
            flate2::read::GzDecoder::new(stored)
                .read_to_end(&mut unpacked)
                .map_err(damaged)?;
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
        assert_eq!(encoding, Encoding::Sealed);
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
        assert_eq!(Encoding::parse("sealed").unwrap(), Encoding::Sealed);
        assert!(Encoding::parse("sideways").is_err());
        assert_eq!(Encoding::Packed.as_str(), "packed");
        assert_eq!(Encoding::Sealed.as_str(), "sealed");
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

    fn deflated(raw: &[u8]) -> Vec<u8> {
        let mut packer =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        packer.write_all(raw).expect("the body packs");
        packer.finish().expect("the body packs")
    }

    #[test]
    fn a_body_an_earlier_release_packed_still_reads() {
        let raw = format!(r#"{{"note":"{}"}}"#, "repeat ".repeat(200)).into_bytes();
        let stored = deflated(&raw);
        assert_eq!(decoded(&stored, Encoding::Packed).unwrap(), raw);
    }

    #[test]
    fn a_sealed_body_that_was_altered_is_reported_rather_than_returned() {
        let raw = format!(r#"{{"note":"{}"}}"#, "repeat ".repeat(200)).into_bytes();
        let (mut stored, encoding) = encoded(&raw);
        assert_eq!(encoding, Encoding::Sealed);
        let last = stored.len() - 1;
        stored[last] ^= 0xff;
        assert!(decoded(&stored, Encoding::Sealed).is_err());
    }

    #[test]
    fn a_sealed_body_cut_short_is_reported_rather_than_returned() {
        let raw = format!(r#"{{"note":"{}"}}"#, "repeat ".repeat(200)).into_bytes();
        let (stored, _) = encoded(&raw);
        let cut = stored[..stored.len() - 3].to_vec();
        assert!(decoded(&cut, Encoding::Sealed).is_err());
        assert!(LazyBody::new(cut, Encoding::Sealed).bytes().is_err());
    }
}
