//! expect_stamp capture/verify — spec §5.2, ports transaction.py:33-51.
//!
//! The stamp is the full `(ino|file_index, mtime_ns, ctime_ns, size)` tuple:
//! same-size rewrites inside coarse mtime granularity are caught by mtime;
//! mtime-preserving `cp -p` copies are caught by inode; symlinks are caught
//! by lstat + the per-component walk. Serialization is explicit-keys-nullable
//! — a component unavailable on the platform serializes as `null`, never an
//! omitted key, and a `null` component degrades to a component-skip at
//! verification time.

use std::fs;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::err::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stamp {
    pub ino: Option<u64>,
    pub mtime_ns: Option<u128>,
    pub ctime_ns: Option<u128>,
    pub size: u64,
}

/// Port of transaction.py `_reject_symlink_components` (lines 42-51):
/// lstat-verify that no existing component of `path` below `root` is a
/// symlink. `root` is the PROJECT root (paths under `.llm-wiki` are walked
/// through it). `..` escapes are rejected.
pub fn check_no_symlink_components(path: &Path, root: &Path) -> Result<(), EngineError> {
    let rel = path.strip_prefix(root).map_err(|_| {
        EngineError::Txn(format!("path escapes wiki root: {}", path.display()))
    })?;
    let mut cur = root.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Normal(part) => {
                cur.push(part);
                match fs::symlink_metadata(&cur) {
                    Ok(md) if md.file_type().is_symlink() => {
                        return Err(EngineError::Txn(format!(
                            "refusing symlinked path component: {}",
                            cur.display()
                        )));
                    }
                    _ => {}
                }
            }
            // Python rejects only leading ".." (anything below root). A
            // literal ParentDir inside an uncleaned path is hostile too.
            Component::ParentDir => {
                return Err(EngineError::Txn(format!(
                    "path escapes wiki root: {}",
                    path.display()
                )))
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    Ok(())
}

impl Stamp {
    /// lstat `path`. `Ok(None)` = absent. A symlinked leaf → Txn error
    /// (`capture` never follows links). For paths under the wiki root,
    /// callers run `check_no_symlink_components` first so ANCESTOR
    /// components are rejected as well.
    pub fn capture(path: &Path) -> Result<Option<Stamp>, EngineError> {
        match fs::symlink_metadata(path) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(EngineError::Txn(format!(
                        "refusing symlinked path: {}",
                        path.display()
                    )));
                }
                Ok(Some(Self::from_meta(&md)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// lstat `path` under the wiki root with the full component walk, then
    /// capture. This is the txn-facing entry point.
    pub fn capture_in(path: &Path, root: &Path) -> Result<Option<Stamp>, EngineError> {
        check_no_symlink_components(path, root)?;
        Self::capture(path)
    }

    /// Re-lstat and compare. `Ok(false)` on absent or any present-component
    /// mismatch; `Err` on a planted symlink or hostile component — the
    /// caller (commit's expect-check) surfaces it as a txn failure, which is
    /// stricter than a plain conflict and matches the spec's symlink
    /// rejection.
    pub fn matches(&self, path: &Path) -> Result<bool, EngineError> {
        let md = match fs::symlink_metadata(path) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if md.file_type().is_symlink() {
            return Err(EngineError::Txn(format!(
                "refusing symlinked path: {}",
                path.display()
            )));
        }
        let cur = Self::from_meta(&md);
        // null-degradation: a component skips the check unless BOTH sides
        // have it. `size` is non-nullable and always compared.
        let ino_eq = match (self.ino, cur.ino) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let mtime_eq = match (self.mtime_ns, cur.mtime_ns) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let ctime_eq = match (self.ctime_ns, cur.ctime_ns) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        Ok(ino_eq && mtime_eq && ctime_eq && self.size == cur.size)
    }

    /// Build a stamp from an already-captured `symlink_metadata` (callers
    /// that stat a path once and reuse the tuple).
    pub fn from_meta(md: &fs::Metadata) -> Stamp {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            return Stamp {
                ino: Some(md.ino()),
                mtime_ns: Some((md.mtime().max(0) as u128) * 1_000_000_000
                    + md.mtime_nsec().max(0) as u128),
                ctime_ns: Some((md.ctime().max(0) as u128) * 1_000_000_000
                    + md.ctime_nsec().max(0) as u128),
                size: md.size(),
            };
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            return Stamp {
                // file_index()/last_change_time() are nightly-only
                // (windows_by_handle); stable build tracks mtime+ctime+size,
                // with no inode anchor on Windows.
                ino: None,
                mtime_ns: Some(md.last_write_time() as u128 * 100), // 100ns → ns
                ctime_ns: Some(md.creation_time() as u128 * 100),
                size: md.file_size(),
            };
        }
        #[cfg(not(any(unix, windows)))]
        {
            let mtime_ns = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos());
            Stamp {
                ino: None,
                mtime_ns,
                ctime_ns: None,
                size: md.len(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_mtime_change(path: &Path, first: &Stamp) {
        // Rewrite same-size content until mtime (or the inode) moves —
        // covers FAT/exFAT-class ~2 s granularity and same-inode rewrites.
        for _ in 0..200 {
            std::fs::write(path, b"bbbbbbbb").unwrap();
            if !first.matches(path).unwrap() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        panic!("stamp never detected rewrite");
    }

    #[test]
    fn stamp_detects_same_size_rewrite_via_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, b"aaaaaaaa").unwrap();
        let s = Stamp::capture(&f).unwrap().unwrap();
        assert!(s.matches(&f).unwrap());
        wait_mtime_change(&f, &s);
        assert!(!s.matches(&f).unwrap());
    }

    #[test]
    fn stamp_detects_mtime_preserving_copy_via_inode() {
        // cp -p preserves mtime+size but lands on a new inode.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, b"data").unwrap();
        let s = Stamp::capture(&f).unwrap().unwrap();
        let src = dir.path().join("src.txt");
        std::fs::write(&src, b"data").unwrap();
        let out = std::process::Command::new("cp")
            .arg("-p")
            .arg(&src)
            .arg(&f)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(!s.matches(&f).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn stamp_rejects_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(Stamp::capture(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stamp_rejects_symlinked_parent_component() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("real/sub")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linkdir")).unwrap();
        let p = root.join("linkdir/new.txt");
        assert!(Stamp::capture_in(&p, &root).is_err());
    }

    #[test]
    fn stamp_null_component_degrades_to_skip() {
        // Stamp{ino:None,..}: verification only checks present fields —
        // same file still matches because size agrees; a changed size fails.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, b"1234").unwrap();
        let s = Stamp {
            ino: None,
            mtime_ns: None,
            ctime_ns: None,
            size: 4,
        };
        assert!(s.matches(&f).unwrap());
        std::fs::write(&f, b"12345").unwrap();
        assert!(!s.matches(&f).unwrap());
    }

    #[test]
    fn stamp_serializes_explicit_nulls() {
        let s = Stamp {
            ino: None,
            mtime_ns: None,
            ctime_ns: None,
            size: 7,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("ino").is_some());
        assert_eq!(v["ino"], serde_json::Value::Null);
        assert_eq!(v["mtime_ns"], serde_json::Value::Null);
        assert_eq!(v["ctime_ns"], serde_json::Value::Null);
        assert_eq!(v["size"], 7);
        // round-trips
        let back: Stamp = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn capture_absent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Stamp::capture(&dir.path().join("nope")).unwrap().is_none());
    }
}
