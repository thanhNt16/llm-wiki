//! ULID-style identifiers — port of wikicore/ids.py verbatim:
//! 48-bit ms timestamp + 80-bit randomness, Crockford base32.

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// `ids._encode` — big-endian Crockford base32, zero-padded to `length`.
pub(crate) fn encode(mut value: u128, length: usize) -> String {
    let mut out = vec![0u8; length];
    for i in (0..length).rev() {
        out[i] = CROCKFORD[(value & 0x1F) as usize];
        value >>= 5;
    }
    String::from_utf8(out).expect("Crockford alphabet is ASCII")
}

/// 80 random bits from the OS CSPRNG (`secrets.randbits(80)`).
fn random_bits() -> u128 {
    use std::io::Read;
    let mut buf = [0u8; 10];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            return u128::from_be_bytes([
                0, 0, 0, 0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                buf[8], buf[9],
            ]);
        }
    }
    // Non-OS fallback (rare): hash time+pid address-space spread. Still 80
    // bits of uniqueness for id-minting purposes; never used for secrets.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = nanos ^ ((std::process::id() as u128) << 64);
    let digest = {
        // FNV-1a 128-bit over the mixed value — sufficient spread for the
        // fallback path; /dev/urandom is the real source on every supported OS.
        let mut h: u128 = 0x6c62272e07bb0142_62b821756295c58d;
        for b in mixed.to_be_bytes() {
            h ^= b as u128;
            h = h.wrapping_mul(0x0000000001000000_000000000000013b);
        }
        h
    };
    digest & ((1u128 << 80) - 1)
}

/// `ids.new(kind)` — fresh id like `claim_01JABC…` (26 ULID chars). Panics
/// on a bad kind, mirroring the Python `ValueError` (callers treat ids as
/// programmer-supplied constants).
pub fn new(kind: &str) -> String {
    let core: String = kind.chars().filter(|c| *c != '_').collect();
    if core.is_empty() || !core.chars().all(|c| c.is_ascii_lowercase()) {
        panic!("kind must be lowercase letters/underscores: {kind:?}");
    }
    let ts = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        & ((1u64 << 48) - 1)) as u128;
    let rand = random_bits() & ((1u128 << 80) - 1);
    format!("{}_{}{}", kind, encode(ts, 10), encode(rand, 16))
}

/// `ids.validate(s)` — `^[a-z]+_[0-9A-HJKMNP-TV-Z]{26}$`. The separator is
/// the FIRST underscore; kind is strictly `[a-z]+` (an underscored kind such
/// as `my_kind_…` mints fine but never validates — Python quirk, kept).
pub fn validate(s: &str) -> bool {
    let Some((kind, code)) = s.split_once('_') else {
        return false;
    };
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_lowercase()) {
        return false;
    }
    code.len() == 26 && code.bytes().all(|b| CROCKFORD.contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn encode_vectors_match_python() {
        // ids._encode ground truth from the Python module.
        assert_eq!(encode(0, 10), "0000000000");
        assert_eq!(encode(1, 10), "0000000001");
        assert_eq!(encode(32, 10), "0000000010");
        assert_eq!(encode((1u128 << 48) - 1, 10), "7ZZZZZZZZZ");
        assert_eq!(encode((1u128 << 80) - 1, 16), "ZZZZZZZZZZZZZZZZ");
    }

    #[test]
    fn new_ids_shape_and_uniqueness() {
        for kind in ["source", "claim", "batch", "run", "a_b"] {
            let id = new(kind);
            assert_eq!(id.len(), kind.len() + 1 + 26);
            assert!(id.starts_with(kind));
        }
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(new("claim")), "collision within 1000 mints");
        }
    }

    #[test]
    fn validate_matches_python_regex() {
        let ok = [
            "x_00000000000000000000000000",
            "claim_ZZZZZZZZZZZZZZZZZZZZZZZZZZ",
            "claim_01JABCDEF0123456789ABCDEF0",
        ];
        for s in ok {
            assert!(validate(s), "{s}");
        }
        let bad = [
            "",
            "claim_",
            "_00000000000000000000000000",
            "claim_0000000000000000000000000",    // 25 chars
            "claim_000000000000000000000000000",  // 27 chars
            "Claim_00000000000000000000000000",   // uppercase kind
            "claim-00000000000000000000000000",   // no underscore sep
            "claim_01IABCDEF0123456789ABCDEF",    // I excluded
            "claim_01LABCDEF0123456789ABCDEF",    // L excluded
            "claim_01OABCDEF0123456789ABCDEF",    // O excluded
            "claim_01UABCDEF0123456789ABCDEF",    // U excluded
            "claim_01uABCDEF0123456789ABCDEF",    // lowercase code
            "claim_01JABCDEF0123456789abcdef",    // lowercase code
            "my_kind_00000000000000000000000000", // underscored kind
            "claim2_00000000000000000000000000",  // digit in kind
            "a_b_00000000000000000000000000",     // kind regex is [a-z]+ only
        ];
        for s in bad {
            assert!(!validate(s), "{s}");
        }
    }

    #[test]
    #[should_panic(expected = "kind must be lowercase letters/underscores")]
    fn new_rejects_bad_kinds() {
        new("Bad");
    }

    #[test]
    #[should_panic(expected = "kind must be lowercase letters/underscores")]
    fn new_rejects_empty_kind() {
        new("");
    }

    #[test]
    #[should_panic(expected = "kind must be lowercase letters/underscores")]
    fn new_rejects_digit_kind() {
        new("claim2");
    }
}
