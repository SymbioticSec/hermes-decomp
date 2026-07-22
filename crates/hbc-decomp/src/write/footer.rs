// Hermes bytecode file footer: SHA-1 over all bytes preceding the footer.

use crate::error::Result;
use sha1::{Digest, Sha1};

pub const FOOTER_LEN: usize = 20;

// Compute the footer hash for `bytes_without_footer` (entire image except the
// trailing hash itself).
pub fn compute_file_hash(bytes_without_footer: &[u8]) -> Result<[u8; FOOTER_LEN]> {
    let digest = Sha1::digest(bytes_without_footer);
    let mut out = [0u8; FOOTER_LEN];
    out.copy_from_slice(&digest);
    Ok(out)
}

// Append the footer to a fully laid-out buffer (headers + sections, no hash).
pub fn append_footer(buf: &mut Vec<u8>) -> Result<()> {
    let hash = compute_file_hash(buf)?;
    buf.extend_from_slice(&hash);
    Ok(())
}

// Recompute and replace the trailing 20-byte SHA-1 footer in place.
// If `buf` is shorter than 20 bytes, append a new footer.
pub fn rehash_footer(buf: &mut Vec<u8>) -> Result<()> {
    if buf.len() >= FOOTER_LEN {
        buf.truncate(buf.len() - FOOTER_LEN);
    }
    append_footer(buf)
}

// True when the last 20 bytes of `buf` equal SHA-1 of the prefix.
pub fn verify_footer(buf: &[u8]) -> bool {
    if buf.len() < FOOTER_LEN {
        return false;
    }
    let (body, foot) = buf.split_at(buf.len() - FOOTER_LEN);
    match compute_file_hash(body) {
        Ok(h) => h.as_slice() == foot,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_matches_real_fixtures() {
        for rel in [
            "../../examples/react-native/v96/expressions/generator/bytecode.hbc",
            "../../examples/react-native/v98/expressions/generator/bytecode.hbc",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
            if !path.exists() {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                verify_footer(&bytes),
                "footer mismatch for {}",
                path.display()
            );
            let mut clone = bytes.clone();
            rehash_footer(&mut clone).unwrap();
            assert_eq!(clone, bytes, "rehash changed identity for {}", path.display());
        }
    }
}
