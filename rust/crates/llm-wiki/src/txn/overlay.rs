//! Staging overlay — spec §5.3 read-your-writes.
//!
//! Every staged op is a `Write` (bytes materialized under
//! `.state/staging/<run_id>/`, never at the destination) or a `Delete`.
//! `state_overlay` holds the not-yet-serialized `stage_state` JSON values so
//! repeated calls compose without O(N²) bytes; commit materializes each
//! staged file exactly once under the flock (transaction.py:186-208).

use std::path::PathBuf;

use serde_json::Value;

/// One ordered staged operation, mirroring transaction.py's `staged` tuples.
#[derive(Debug)]
pub enum StagedOp {
    /// rel → staging path holding the bytes to apply.
    Write { rel: String, src: PathBuf },
    /// rel to unlink at apply time.
    Delete { rel: String },
}

impl StagedOp {
    pub fn rel(&self) -> &str {
        match self {
            StagedOp::Write { rel, .. } | StagedOp::Delete { rel } => rel,
        }
    }

    /// `(op, rel)` as it appears in the journal `ops` list.
    pub fn journal_pair(&self) -> [String; 2] {
        match self {
            StagedOp::Write { rel, .. } => ["write".into(), rel.clone()],
            StagedOp::Delete { rel } => ["delete".into(), rel.clone()],
        }
    }
}

/// Port of `_clean_rel` (transaction.py:373-380): strip slashes, drop empty
/// and `.` components. `..` is preserved here and rejected later by the
/// symlink-component walk, exactly like the Python engine.
pub fn clean_rel(rel: &str) -> String {
    rel.trim_matches('/')
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Last write to the same rel wins — replaces an existing op of the SAME
/// kind for that rel (transaction.py `_replace_or_append`).
pub fn replace_or_append(staged: &mut Vec<StagedOp>, op: StagedOp) {
    let same_kind = |a: &StagedOp, b: &StagedOp| {
        matches!(
            (a, b),
            (StagedOp::Write { .. }, StagedOp::Write { .. })
                | (StagedOp::Delete { .. }, StagedOp::Delete { .. })
        )
    };
    for i in 0..staged.len() {
        if same_kind(&staged[i], &op) && staged[i].rel() == op.rel() {
            staged[i] = op;
            return;
        }
    }
    staged.push(op);
}

/// Serialize an overlay JSON value to its staging path — the exact bytes
/// eager `stage_state` produced in Python (`json.dumps(indent=2) + "\n"`).
pub fn write_state_file(src: &PathBuf, data: &Value) -> Result<(), crate::err::EngineError> {
    if let Some(parent) = src.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(src, format!("{}\n", serde_json::to_string_pretty(data)?))?;
    Ok(())
}

/// Last staged op wins for overlay reads: after a `Delete` the file is
/// gone to the txn even if an earlier `Write` op is still queued.
pub fn last_op_for<'a>(staged: &'a [StagedOp], rel: &str) -> Option<&'a StagedOp> {
    staged.iter().rev().find(|op| op.rel() == rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_rel_strips_slashes_and_dots() {
        assert_eq!(clean_rel("/claims/./c1.json/"), "claims/c1.json");
        assert_eq!(clean_rel("a//b"), "a/b");
        assert_eq!(clean_rel(""), "");
    }

    #[test]
    fn replace_or_append_last_write_wins() {
        let mut staged = vec![StagedOp::Write {
            rel: "a".into(),
            src: PathBuf::from("/x/a"),
        }];
        replace_or_append(
            &mut staged,
            StagedOp::Write {
                rel: "a".into(),
                src: PathBuf::from("/y/a"),
            },
        );
        assert_eq!(staged.len(), 1);
        match &staged[0] {
            StagedOp::Write { src, .. } => assert_eq!(src, &PathBuf::from("/y/a")),
            _ => panic!(),
        }
        replace_or_append(&mut staged, StagedOp::Delete { rel: "a".into() });
        assert_eq!(staged.len(), 2); // write-then-delete parity
        assert!(matches!(last_op_for(&staged, "a"), Some(StagedOp::Delete { .. })));
    }
}
