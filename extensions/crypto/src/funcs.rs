//! Pure-Rust crypto / encoding implementations. Native-testable.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

/// Hash `bytes` with SHA-1 and return the digest as lowercase
/// hex.
pub fn sha1(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn sha256(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn sha512(bytes: &[u8]) -> String {
    let mut h = Sha512::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn md5(bytes: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn hex_encode(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    // SQLite's unhex() tolerates leading/trailing whitespace; do
    // the same to keep parity.
    let trimmed = s.trim();
    hex::decode(trimmed).map_err(|e| e.to_string())
}

pub fn base64_encode(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    B64.decode(s.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_empty() {
        assert_eq!(sha1(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn sha256_abc() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha512_empty() {
        assert_eq!(
            sha512(b""),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    #[test]
    fn md5_empty() {
        assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn hex_round_trip() {
        let bytes = vec![0xde, 0xad, 0xbe, 0xef];
        assert_eq!(hex_encode(&bytes), "deadbeef");
        assert_eq!(hex_decode("deadbeef").unwrap(), bytes);
    }

    #[test]
    fn base64_round_trip() {
        let bytes = b"hello world";
        let enc = base64_encode(bytes);
        assert_eq!(enc, "aGVsbG8gd29ybGQ=");
        assert_eq!(base64_decode(&enc).unwrap(), bytes);
    }

    #[test]
    fn unhex_tolerates_whitespace() {
        assert_eq!(hex_decode("  deadbeef\n").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
    }
}
