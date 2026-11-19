//! Atomic file io + clock primitives moved with the store/txn core from
//! the CLI crate's `util.rs` (the CLI re-exports these, so `crate::util`
//! call sites are unchanged). `atomic_write` deliberately reuses a fixed
//! `<name>.tmp` sibling (store.py parity) — commits are serialized by
//! `commit.lock`, so the fixed name can never collide in-flight.
use std::{fs, io::Write, path::Path};

/// Write `bytes` to `path` via a sibling tmp file + rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.parent().unwrap().join(format!(
        "{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    fs::File::create(&tmp)?.write_all(bytes)?;
    fs::rename(&tmp, path)
}

/// Pretty-printed JSON (indent 2, trailing newline) written atomically —
/// matches the Python engine's `json.dump(indent=2)` + `f.write("\n")`.
pub fn write_json_atomic(path: &Path, v: &serde_json::Value) -> std::io::Result<()> {
    atomic_write(
        path,
        format!("{}\n", serde_json::to_string_pretty(v)?).as_bytes(),
    )
}

/// RFC3339 UTC timestamp — `time.strftime("%Y-%m-%dT%H:%M:%SZ", gmtime())` parity.
pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        mo,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// `time.strftime("%Y-%m-%d", gmtime())` — today's UTC calendar date.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{mo:02}-{d:02}")
}

/// Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

pub fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// atomic_write with the §5.2 symlink defense: lstat-walk every component
/// of `path` under `root` (leaf included) before writing.
pub fn atomic_write_checked(
    path: &Path,
    root: &Path,
    bytes: &[u8],
) -> Result<(), crate::err::EngineError> {
    crate::txn::stamps::check_no_symlink_components(path, root)?;
    atomic_write(path, bytes).map_err(crate::err::EngineError::from)
}
