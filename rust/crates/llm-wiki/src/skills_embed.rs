//! Embedded skill tree + `--install-skills` (spec §12).
//!
//! `build.rs` materializes the *binary variant* of the repo `skill/` tree
//! into `$OUT_DIR/skill-binary/` (minus `scripts/`, SKILL.md/COMMANDS.md
//! rewritten to `llm-wiki <cmd>` invocations, `graph-ui/dist/` hoisted for
//! viz-serve). `EmbeddedSkills` embeds that whole tree; `install` copies the
//! `skill/<name>/` subset into an agent skills directory.
//!
//! Refresh semantics (spec §12): files belonging to the embedded set are
//! removed then rewritten (tmp+rename); files inside the skill dir that are
//! not in the embedded set are left untouched — users' added files survive
//! a refresh. Installed copies carry the `.llm-wiki-installed` marker so
//! `install.sh --uninstall` recognizes them as managed.

use std::fs;
use std::path::{Path, PathBuf};

use rust_embed::Embed;

use crate::err::EngineError;

/// The materialized binary-variant tree (`skill-binary/` from build.rs).
#[derive(Embed)]
#[folder = "$OUT_DIR/skill-binary"]
pub struct EmbeddedSkills;

/// Marker install.sh drops in copied skill dirs — written here too so
/// `--uninstall` treats binary-installed skills as managed copies.
const MARKER: &str = ".llm-wiki-installed";

/// Preset skill target directories (spec §12: `--agents`/`--cellockai`).
#[derive(Clone, Copy, Debug)]
pub enum Preset {
    /// ~/.omp/agent/skills (install.sh default target)
    Omp,
    /// ~/.agents/skills (--agents)
    Agents,
    /// ~/.cellockai/skills (--cellockai)
    Cellockai,
}

impl Preset {
    pub fn dir(self, home: &Path) -> PathBuf {
        match self {
            Self::Omp => home.join(".omp/agent/skills"),
            Self::Agents => home.join(".agents/skills"),
            Self::Cellockai => home.join(".cellockai/skills"),
        }
    }
}

/// Names of the embedded skills, sorted (embedded paths are
/// `skill/<name>/…`).
pub fn skill_names() -> Vec<String> {
    let mut names: Vec<String> = EmbeddedSkills::iter()
        .filter_map(|p| {
            let mut it = p.split('/');
            if it.next() == Some("skill") {
                it.next().map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Look up an embedded file under the `graph-ui/dist/` tree — the tree
/// `viz-serve`/`graph-export` serve when no `--ui-dist` override exists
/// (spec §12: served from embedded memory, never extracted to disk).
pub fn graph_ui_file(rel: &str) -> Option<rust_embed::EmbeddedFile> {
    EmbeddedSkills::get(&format!("graph-ui/dist/{rel}"))
}

/// Copy-mode install of every embedded skill into `<target>/<name>/`.
/// Returns `{"installed": [names], "target": <dir>, "count": N}`.
pub fn install(target: &Path) -> Result<serde_json::Value, EngineError> {
    fs::create_dir_all(target).map_err(|e| {
        EngineError::Txn(format!("create {}: {e}", target.display()))
    })?;
    let names = skill_names();
    for name in &names {
        let skill_dir = target.join(name);
        let mut managed: Vec<String> = EmbeddedSkills::iter()
            .filter_map(|p| {
                p.strip_prefix(&format!("skill/{name}/"))
                    .filter(|rest| !rest.is_empty())
                    .map(|rest| rest.to_string())
            })
            .collect();
        managed.sort();
        for rel in managed {
            let file = EmbeddedSkills::get(&format!("skill/{name}/{rel}"))
                .ok_or_else(|| {
                    EngineError::Txn(format!("embedded file vanished: skill/{name}/{rel}"))
                })?;
            write_replaced(&skill_dir.join(&rel), &file.data)?;
        }
        // Managed marker: install.sh --uninstall parity.
        write_replaced(&skill_dir.join(MARKER), b"")?;
    }
    Ok(serde_json::json!({
        "installed": names,
        "target": target.to_string_lossy(),
        "count": names.len(),
    }))
}

/// Write `bytes` to `path` via tmp+rename in the same directory, removing a
/// stale target first (spec §12 remove-and-replace). Parent dirs are
/// created; unmanaged siblings are untouched.
fn write_replaced(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)
        .map_err(|e| EngineError::Txn(format!("create {}: {e}", parent.display())))?;
    if path.symlink_metadata().is_ok() {
        // remove-and-replace: stale managed file (or a plant) goes first.
        if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
        .map_err(|e| EngineError::Txn(format!("remove {}: {e}", path.display())))?;
    }
    let tmp = parent.join(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    fs::write(&tmp, bytes)
        .map_err(|e| EngineError::Txn(format!("write {}: {e}", tmp.display())))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        EngineError::Txn(format!("rename {}: {e}", path.display()))
    })?;
    Ok(())
}
