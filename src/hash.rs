//! Content hashes: SHA-256 is a document's identity in the index; MD5 is
//! the hash Google Drive keeps, so a file can be checked without a
//! download.

use std::io::Read;

use md5::Md5;
use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn md5_hex(bytes: &[u8]) -> String {
    hex(&Md5::digest(bytes))
}

/// SHA-256 and MD5 of a reader in one pass, with the byte count.
pub fn both_reader(mut r: impl Read) -> std::io::Result<(String, String, u64)> {
    let mut s = Sha256::new();
    let mut m = Md5::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        s.update(&buf[..n]);
        m.update(&buf[..n]);
    }
    Ok((hex(&s.finalize()), hex(&m.finalize()), total))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let (s, m, n) = both_reader(&b"abc"[..]).unwrap();
        assert_eq!(
            (s.len(), m.as_str(), n),
            (64, "900150983cd24fb0d6963f7d28e17f72", 3)
        );
    }
}
