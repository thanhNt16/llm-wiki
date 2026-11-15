//! Commit lock — spec §5.6. `commit.lock` is NEVER deleted: kernel flock
//! releases on fd close / process death, so it cannot be stale; unlinking
//! it would create an unlink-and-recreate race where two processes hold
//! locks on different inodes.

use std::fs;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

use crate::err::EngineError;
use crate::store::Wiki;

pub struct Lock {
    file: fs::File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl Lock {
    /// `try_lock_exclusive` on `.state/locks/commit.lock` — the file and its
    /// parent are created if absent. `Err(EngineError::Locked)` (exit 5) on
    /// contention, mirroring fcntl EACCES/EAGAIN.
    pub fn acquire(wiki: &Wiki) -> Result<Lock, EngineError> {
        let (file, path) = open_lock_file(wiki)?;
        // fs4 returns Ok(false) on a would-block EWOULDBLOCK — map both the
        // false verdict and a hard error to Locked (fcntl EACCES parity).
        match file.try_lock_exclusive() {
            Ok(true) => {}
            Ok(false) => return Err(EngineError::Locked),
            Err(e) if would_block(&e) => return Err(EngineError::Locked),
            Err(e) => return Err(EngineError::Txn(format!("commit lock: {}", e))),
        }
        Ok(Lock { file, path })
    }

    /// `--wait-lock` pre-check — wiki.py `wait_for_lock` parity. Waits until
    /// an in-flight commit releases `commit.lock`, releases it, and returns.
    /// Deliberately TOCTOU: `Txn::commit` still `try_lock`s itself; this only
    /// rides out a known in-flight commit, never guarantees the real commit
    /// succeeds. `timeout: None` blocks on the kernel flock until released;
    /// `Some(d)` polls `try_lock` until `d` elapses, then
    /// `Err(EngineError::Locked)` (exit 5).
    pub fn wait_until_free(wiki: &Wiki, timeout: Option<Duration>) -> Result<(), EngineError> {
        let (file, _path) = open_lock_file(wiki)?;
        match timeout {
            None => {
                file.lock_exclusive()
                    .map_err(|e| EngineError::Txn(format!("commit lock: {}", e)))?;
            }
            Some(limit) => {
                let deadline = Instant::now() + limit;
                loop {
                    match file.try_lock_exclusive() {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(e) if would_block(&e) => {}
                        Err(e) => {
                            return Err(EngineError::Txn(format!("commit lock: {}", e)))
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(EngineError::Locked);
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        let _ = file.unlock();
        Ok(())
    }

    /// Blocking acquire for `init`: scaffold writers don't contend on
    /// commits, they SERIALIZE with them — kernel-block until the flock is
    /// free (released on the holder's fd close/process death, so it can
    /// never wedge) rather than exiting 5 on a mid-init collision.
    pub fn acquire_blocking(wiki: &Wiki) -> Result<Lock, EngineError> {
        let (file, path) = open_lock_file(wiki)?;
        file.lock_exclusive()
            .map_err(|e| EngineError::Txn(format!("commit lock: {}", e)))?;
        Ok(Lock { file, path })
    }
}

/// Open `.state/locks/commit.lock` for locking: create the file and parent
/// if absent, refuse a planted symlink (O_NOFOLLOW parity with
/// transaction.py `_open_nofollow`).
fn open_lock_file(wiki: &Wiki) -> Result<(fs::File, PathBuf), EngineError> {
    let path = wiki.p(".state/locks/commit.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(md) = fs::symlink_metadata(&path) {
        if md.file_type().is_symlink() {
            return Err(EngineError::Txn(format!(
                "refusing symlinked lock file: {}",
                path.display()
            )));
        }
    }
    let file = OpenOptions::new().read(true).write(true).create(true).open(&path)?;
    Ok((file, path))
}

fn would_block(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    {
        // fcntl parity: EAGAIN/EWOULDBLOCK (11) and EACCES (13) both mean
        // "lock held"; ErrorKind mapping varies by crate version.
        matches!(e.raw_os_error(), Some(11) | Some(13))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
