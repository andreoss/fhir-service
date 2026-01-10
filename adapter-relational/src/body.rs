use fhir_core::Error;

pub fn decoded(stored: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(stored.to_vec())
}

pub fn encoded(raw: &[u8]) -> Vec<u8> {
    raw.to_vec()
}
