//! Commitment-reveal pairing code (architecture §10.4).

use sha2::{Digest, Sha256};

/// `SHA-256("CSC-PAIR-COMMIT-v2" ‖ Na ‖ device_id)`.
pub fn pair_commit(na: &[u8], device_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"CSC-PAIR-COMMIT-v2");
    hasher.update(na);
    hasher.update(device_id.as_bytes());
    hasher.finalize().into()
}

/// Constant-time check that `commit` is `pair_commit(na, device_id)`.
pub fn pair_commit_matches(commit: &[u8], na: &[u8], device_id: &str) -> bool {
    let expect = pair_commit(na, device_id);
    if commit.len() != expect.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in commit.iter().zip(expect.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Six-digit code: `u32_be(SHA-256("CSC-SAS-v2" ‖ E ‖ Na ‖ Nb)[0..4]) % 1_000_000`.
/// `exporter` is `export_keying_material(b"EXPORTER-CSC-SAS-v2", 32)`.
pub fn pair_code(exporter: &[u8], na: &[u8], nb: &[u8]) -> u32 {
    let mut hasher = Sha256::new();
    hasher.update(b"CSC-SAS-v2");
    hasher.update(exporter);
    hasher.update(na);
    hasher.update(nb);
    let dig = hasher.finalize();
    u32::from_be_bytes([dig[0], dig[1], dig[2], dig[3]]) % 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_hides_the_nonce_until_it_is_revealed() {
        let na = [7u8; 32];
        let commit = pair_commit(&na, "host-id");
        assert!(pair_commit_matches(&commit, &na, "host-id"));
        assert!(!pair_commit_matches(&commit, &na, "other"));
        let mut wrong = na;
        wrong[0] ^= 1;
        assert!(!pair_commit_matches(&commit, &wrong, "host-id"));
    }

    #[test]
    fn code_is_six_digits_and_depends_on_both_nonces() {
        let e = [1u8; 32];
        let na = [2u8; 32];
        let nb = [3u8; 32];
        let code = pair_code(&e, &na, &nb);
        assert!(code < 1_000_000);
        assert_ne!(code, pair_code(&e, &nb, &na));
        assert_eq!(code, pair_code(&e, &na, &nb));
    }
}
