//! Hashing: transaction IDs and the data manifest use SHA-256.

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The first `len` characters of [`sha256_hex`] (all 64, if `len` is more),
/// for a name that only has to be unlikely to be taken.
pub fn sha256_hex_start(bytes: &[u8], len: usize) -> String {
    sha256_hex(bytes).chars().take(len).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // SHA-256 of "abc", from the standard that defines it (FIPS 180-4).
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn the_start_of_a_hash_is_its_first_characters() {
        assert_eq!(sha256_hex(b"abc"), ABC);
        assert_eq!(sha256_hex_start(b"abc", 6), "ba7816");
        assert_eq!(sha256_hex_start(b"abc", 0), "");
        assert_eq!(sha256_hex_start(b"abc", 100), ABC);
    }
}
