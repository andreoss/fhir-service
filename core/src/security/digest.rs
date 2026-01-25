const ROUND: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const START: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const BLOCK: usize = 64;

fn block(state: &mut [u32; 8], chunk: &[u8]) {
    let mut words = [0u32; 64];
    for (index, word) in words.iter_mut().enumerate().take(16) {
        let at = index * 4;
        *word = u32::from_be_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]]);
    }
    for index in 16..64 {
        let low = words[index - 15];
        let high = words[index - 2];
        let mixed_low = low.rotate_right(7) ^ low.rotate_right(18) ^ (low >> 3);
        let mixed_high = high.rotate_right(17) ^ high.rotate_right(19) ^ (high >> 10);
        words[index] = words[index - 16]
            .wrapping_add(mixed_low)
            .wrapping_add(words[index - 7])
            .wrapping_add(mixed_high);
    }
    let mut work = *state;
    for index in 0..64 {
        let spread = work[4].rotate_right(6) ^ work[4].rotate_right(11) ^ work[4].rotate_right(25);
        let pick = (work[4] & work[5]) ^ (!work[4] & work[6]);
        let first = work[7]
            .wrapping_add(spread)
            .wrapping_add(pick)
            .wrapping_add(ROUND[index])
            .wrapping_add(words[index]);
        let widen = work[0].rotate_right(2) ^ work[0].rotate_right(13) ^ work[0].rotate_right(22);
        let carry = (work[0] & work[1]) ^ (work[0] & work[2]) ^ (work[1] & work[2]);
        let second = widen.wrapping_add(carry);
        work[7] = work[6];
        work[6] = work[5];
        work[5] = work[4];
        work[4] = work[3].wrapping_add(first);
        work[3] = work[2];
        work[2] = work[1];
        work[1] = work[0];
        work[0] = first.wrapping_add(second);
    }
    for (slot, added) in state.iter_mut().zip(work.iter()) {
        *slot = slot.wrapping_add(*added);
    }
}

pub fn sha256(message: &[u8]) -> [u8; 32] {
    let mut state = START;
    let mut chunks = message.chunks_exact(BLOCK);
    for chunk in chunks.by_ref() {
        block(&mut state, chunk);
    }
    let mut tail = chunks.remainder().to_vec();
    tail.push(0x80);
    while tail.len() % BLOCK != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&((message.len() as u64) * 8).to_be_bytes());
    for chunk in tail.chunks_exact(BLOCK) {
        block(&mut state, chunk);
    }
    let mut digest = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut folded = [0u8; BLOCK];
    match key.len() > BLOCK {
        true => folded[..32].copy_from_slice(&sha256(key)),
        false => folded[..key.len()].copy_from_slice(key),
    }
    let inner: Vec<u8> = folded.iter().map(|byte| byte ^ 0x36).collect();
    let outer: Vec<u8> = folded.iter().map(|byte| byte ^ 0x5c).collect();
    let mut first = inner;
    first.extend_from_slice(message);
    let mut second = outer;
    second.extend_from_slice(&sha256(&first));
    sha256(&second)
}

pub fn same(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .fold(0u8, |seen, (one, other)| seen | (one ^ other))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn the_digest_of_the_empty_message_is_the_published_one() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_digest_matches_the_published_vector() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_long_message_spans_more_than_one_block() {
        let message = vec![b'a'; 1000];
        assert_eq!(
            hex(&sha256(&message)),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn the_keyed_digest_matches_the_published_vector() {
        assert_eq!(
            hex(&hmac_sha256(b"key", b"The quick brown fox jumps over the lazy dog")),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn a_key_longer_than_the_block_is_folded_first() {
        let key = vec![b'k'; 200];
        let first = hmac_sha256(&key, b"payload");
        let second = hmac_sha256(&key, b"payload");
        assert_eq!(first, second);
        assert_ne!(first, hmac_sha256(b"kk", b"payload"));
    }

    #[test]
    fn digests_compare_equal_only_when_they_match() {
        assert!(same(&sha256(b"a"), &sha256(b"a")));
        assert!(!same(&sha256(b"a"), &sha256(b"b")));
        assert!(!same(&sha256(b"a")[..4], &sha256(b"a")));
    }
}
