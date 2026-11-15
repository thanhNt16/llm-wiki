//! ULID-style identifiers — port of wikicore/ids.py verbatim:
//! 48-bit ms timestamp + 80-bit randomness, Crockford base32.

use std::sync::atomic::{AtomicU64, Ordering};

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn encode(mut value: u128, length: usize) -> String {
    let mut out = vec![0u8; length];
    for i in (0..length).rev() {
        out[i] = CROCKFORD[(value & 0x1F) as usize];
        value >>= 5;
    }
    String::from_utf8(out).unwrap()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_bits() -> u128 {
    use std::io::Read;
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            return u128::from_be_bytes(buf);
        }
    }
    // Fallback: hash-of-time/counter/pid spread across 128 bits.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = nanos
        ^ ((COUNTER.fetch_add(1, Ordering::Relaxed) as u128) << 64)
        ^ ((std::process::id() as u128) << 32);
    u128::from_be_bytes(crate::util::sha256_hex(&mixed.to_be_bytes())
        .as_bytes()[..16]
        .try_into()
        .unwrap())
}

/// `ids.new(kind)` → `<kind>_<26 Crockford chars>` e.g. `claim_01JABC...`.
/// kind must be lowercase letters/underscores (ids.py:new).
pub fn new(kind: &str) -> String {
    let core: String = kind.chars().filter(|c| *c != '_').collect();
    if core.is_empty() || !core.chars().all(|c| c.is_ascii_lowercase()) {
        panic!("kind must be lowercase letters/underscores: {kind:?}");
    }
    let ts = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        & ((1 << 48) - 1)) as u128;
    let rand = random_bits() & ((1u128 << 80) - 1);
    format!("{}_{}{}", kind, encode(ts, 10), encode(rand, 16))
}

/// `ids.validate(s)` — `^[a-z]+_[0-9A-HJKMNP-TV-Z]{26}$`.
pub fn validate(s: &str) -> bool {
    let (kind, code) = match s.rsplit_once('_') {
        Some(t) => t,
        None => return false,
    };
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_lowercase()) {
        return false;
    }
    code.len() == 26 && code.bytes().all(|b| CROCKFORD.contains(&b))
}

/// Mint a run id (`run_<ulid>`) — callers pass to `Txn::begin`.
pub fn new_run() -> String {
    new("run")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_validate() {
        for kind in ["source", "claim", "run", "text", "decision"] {
            let id = new(kind);
            assert!(validate(&id), "{id}");
            assert!(id.starts_with(&format!("{kind}_")));
            assert_eq!(id.len(), kind.len() + 1 + 26);
        }
        assert!(!validate("bad"));
        assert!(!validate("Source_01JABCDEFGHIJKMNPQRSTVWXYZ"));
        assert!(!validate("claim_toolong_01JABCDEFGHIJKMNPQRSTVWXYZ!"));
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(new("run"), new("run"));
    }
}
