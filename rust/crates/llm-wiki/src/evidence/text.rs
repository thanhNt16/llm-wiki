//! Text-like ingest lane — `_is_text_like` + the markitdown probe.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::evidence::{ext, OFFICE_EXTS};

/// `_is_text_like` (sources.py:71-84): kind in (text,url,session) → true;
/// kind=file → false for office exts / NUL bytes / invalid UTF-8.
pub fn is_text_like(kind: &str, path_or_ref: &str, data: &[u8]) -> bool {
    match kind {
        "text" | "url" | "session" => true,
        "file" => {
            if OFFICE_EXTS.contains(&ext(path_or_ref).as_str()) {
                return false;
            }
            if data.contains(&0) {
                return false;
            }
            std::str::from_utf8(data).is_ok()
        }
        _ => false, // directory/repo handled as binary-ish bundle
    }
}

/// `shutil.which` equivalent: executable file present on PATH.
fn which(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let p = dir.join(name);
        if p.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if p.metadata()
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
                {
                    return true;
                }
            }
            #[cfg(not(unix))]
            return true;
        }
    }
    false
}

/// `_parser_for_binary` (sources.py:117-127): run `markitdown <path>` when the
/// binary is on PATH. Returns (text, "markitdown") or None — never hard-dep.
/// timeout=120s parity.
pub fn parser_for_binary(path: &Path) -> Option<(String, String)> {
    if !which("markitdown") {
        return None;
    }
    let child = Command::new("markitdown")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return None,
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Ok(None) => {
                let _ = child.kill();
                return None;
            }
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }
    let mut buf = Vec::new();
    use std::io::Read;
    if let Some(mut out) = child.stdout.take() {
        if out.read_to_end(&mut buf).is_err() {
            return None;
        }
    }
    Some((String::from_utf8_lossy(&buf).into_owned(), "markitdown".into()))
}

/// Probe target for markitdown (sources.py `_staging_raw_path` parity):
/// the txn-staged raw file when present, else the origin file on disk,
/// else a tempfile under `.state/staging/_probe/` that preserves the
/// extension (markitdown dispatches on it). Owned paths are removed on drop.
pub fn probe_path(
    wiki_dot: &Path,
    origin: &str,
    data: &[u8],
    staged: Option<std::path::PathBuf>,
) -> std::io::Result<ProbePath> {
    if let Some(p) = staged {
        if p.is_file() {
            return Ok(ProbePath::Borrowed(p));
        }
    }
    let p = Path::new(origin);
    if p.is_file() {
        return Ok(ProbePath::Borrowed(p.to_path_buf()));
    }
    let dir = wiki_dot.join(".state/staging/_probe");
    std::fs::create_dir_all(&dir)?;
    let name = format!("probe-{}{}", std::process::id(), ext(origin).as_str());
    let tmp = dir.join(name);
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data)?;
    Ok(ProbePath::Owned(tmp))
}
pub enum ProbePath {
    Borrowed(std::path::PathBuf),
    Owned(std::path::PathBuf),
}

impl ProbePath {
    pub fn path(&self) -> &Path {
        match self {
            Self::Borrowed(p) | Self::Owned(p) => p,
        }
    }
}

impl Drop for ProbePath {
    fn drop(&mut self) {
        if let Self::Owned(p) = self {
            let _ = std::fs::remove_file(p);
        }
    }
}
