//! Small shared primitives: sibling-tmp `atomic_write` (+ the §5.2
//! symlink-checked variant), sha256, RFC3339 UTC clock without a date
//! dependency. `atomic_write` deliberately reuses a fixed `<name>.tmp`
//! sibling (store.py parity) — commits are serialized by `commit.lock`,
//! so the fixed name can never collide in-flight.
use std::{fs, io::Write, path::Path};
use serde_json::Value;

/// Write `bytes` to `path` via a sibling tmp file + rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.parent().unwrap().join(format!(
        "{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    fs::File::create(&tmp)?.write_all(bytes)?;
    fs::rename(&tmp, path)
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// MD5 hex digest — the slug-collision suffix contract
/// (synthesize.py: `hashlib.md5(subject).hexdigest()[:6]`). Implemented
/// inline to avoid a new crate dependency.
pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, //
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, //
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, //
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = String::with_capacity(32);
    for w in [a0, b0, c0, d0] {
        out.push_str(&format!("{:08x}", w.swap_bytes()));
    }
    out
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

/// `time.strptime(ts, "%Y-%m-%dT%H:%M:%SZ")` → epoch seconds; `None` on
/// any malformed input (strict shape + range checks, no normalization).
pub fn parse_z_epoch(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |a: usize, c: usize| -> Option<i64> {
        ts.get(a..c)?.parse().ok()
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..62).contains(&s)
    {
        return None;
    }
    // days-from-civil (Hinnant, inverse of civil_from_days).
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s)
}

/// Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

/// Python `str()` semantics for a JSON value — used where the Python engine
/// interpolates a claim `value`/`origin` into text (`"%s" % v`, `str(v)`):
/// Null→"None", bools title-case, floats keep `.0`, strings pass through.
pub fn py_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "None".to_string(),
        serde_json::Value::Bool(b) => {
            if *b { "True" } else { "False" }.to_string()
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.is_finite() {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                }
            } else {
                n.to_string()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => format!(
            "[{}]",
            a.iter().map(py_str).collect::<Vec<_>>().join(", ")
        ),
        serde_json::Value::Object(o) => {
            let mut parts: Vec<(&String, &serde_json::Value)> = o.iter().collect();
            parts.sort_by(|a, b| a.0.cmp(b.0));
            format!(
                "{{{}}}",
                parts
                    .iter()
                    .map(|(k, v)| format!("'{k}': {}", py_str(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

// --- export-bundle staging (okf.rs / vizexport.rs) ----------------------

/// `tempfile.mkdtemp(prefix)` — sibling dir of `parent` so the final
/// rename stays on one filesystem (the Python exports rely on `os.rename`
/// atomicity). Caller cleans via [`TmpDir`] or renames the dir away.
pub fn make_tmp_dir(prefix: &str, parent: &Path) -> std::io::Result<std::path::PathBuf> {
    for attempt in 0..100u32 {
        let cand = parent.join(format!("{prefix}{}{attempt}", std::process::id()));
        match fs::create_dir(&cand) {
            Ok(()) => return Ok(cand),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "could not create export temp dir",
    ))
}

/// RAII cleanup for a staging dir — mirrors Python's `finally: rmtree(tmp)`.
/// A tmp dir already renamed into place makes drop a no-op.
pub struct TmpDir(pub std::path::PathBuf);
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Atomic replace of `dest` by `tmp` (okf.py:144-157 / vizexport.py:52-64):
/// previous bundle moves aside to `<dest>.stale-<pid>`, new one renames in;
/// a failed rename restores the old bundle — no data loss.
pub fn atomic_replace_dir(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    let stale = dest
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!(
            "{}.stale-{}",
            dest.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
    let had_old = dest.exists();
    if had_old {
        fs::rename(dest, &stale)?;
    }
    if let Err(e) = fs::rename(tmp, dest) {
        if had_old {
            let _ = fs::rename(&stale, dest);
        }
        return Err(e);
    }
    if had_old {
        let _ = fs::remove_dir_all(&stale);
    }
    Ok(())
}

/// `os.walk` — every regular file under `dir` (dest missing → empty).
pub fn collect_files(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for e in fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if e.file_type()?.is_dir() {
            out.extend(collect_files(&p)?);
        } else {
            out.push(p);
        }
    }
    Ok(out)
}

/// `shutil.copytree(src, dst, dirs_exist_ok=True)` — recursive copy;
/// `dst` may already exist.
pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
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
/// Python `json.dumps(obj, ensure_ascii=False)` with default separators
/// — the serialization `--scope` matches against (wiki.py:621).
pub fn py_json_dumps(v: &Value) -> String {
    let mut out = String::new();
    py_dump(v, &mut out);
    out
}

fn py_dump(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_dump(item, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(k, out);
                out.push_str(": ");
                py_dump(val, out);
            }
            out.push('}');
        }
    }
}

fn dump_str(s: &str, out: &mut String) {
    // Python ensure_ascii=False escaping: " \ and the C0 controls.
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
/// hashing.py `canonical_json` — sorted keys, tight separators, UTF-8.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    canon(v, &mut out);
    out
}

fn canon(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon(item, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_str(k, out);
                out.push(':');
                canon(val, out);
            }
            out.push('}');
        }
    }
}


