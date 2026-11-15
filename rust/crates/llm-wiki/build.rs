//! Build-time materialization of the *binary skill variant* (spec §12).
//!
//! Post-P6 flip: the repo `skill/` tree **is** the binary variant — the
//! generated text from the pre-flip rewrite, landed as a checked-in
//! rewrite. `rewrite_md` is therefore a *no-op* on well-formed repo
//! sources (verified: `diff -rq $OUT_DIR/skill-binary/skill skill -x
//! scripts` is empty). The rewrite rules stay in place as a safety net —
//! they re-normalize any python-form phrasing that drifts back in — and
//! `scripts/` is still excluded from the embedded tree (Python engine =
//! dev-reference, not shipped in the binary).
//!
//!   skill/<name>/…   repo skill/<name>/ minus `scripts/` — SKILL.md,
//!                    references/, agents/, schemas/, assets/ verbatim
//!                    (apply-once-idempotent `.md` rewrites below)
//!   graph-ui/dist/   copy of skill/wiki-visualize/assets/graph-ui/dist/
//!                    so viz-serve/graph-export resolve one embedded tree
//!
//! `.md` rewrite (mechanical, deterministic, idempotent — panics on drift
//! so the variant can never silently go stale):
//!   - the `WIKI=$(ls …)` / `[ -f "$WIKI" ]` path-probe lines are deleted
//!   - `python3 "$WIKI" <cmd>` → `llm-wiki <cmd>`
//!   - bare `wiki.py` tokens → `llm-wiki` (except inside `scripts/wiki.py`
//!     paths, which refer to the repo's dev tree)
//!   - a handful of exact prose replacements scope the engine/wizardry
//!     text to the PATH-installed binary (see PROSE below).
//!
//! No external tools; pure string ops on UTF-8 sources.

use std::fs;
use std::path::{Path, PathBuf};

/// Exact `(from, to)` prose rewrites, applied before the generic line/token
/// passes. Each `from` must appear verbatim in the repo source today; the
/// build panics if a *checked* pair is absent (drift detection) — pairs are
/// checked per-file only when the file contains their anchor token.
const PROSE: &[(&str, &str, &str)] = &[
    // (anchor, from, to) — `from` is replaced only in files containing `anchor`.
    (
        "scripts/wiki.py`. Resolve once:",
        "Every state change goes through the CLI at `scripts/wiki.py`. Resolve once:",
        "Every state change goes through the `llm-wiki` binary on your `PATH`:",
    ),
    (
        "scripts/wiki.py` ships next to it",
        "`$SKILL_DIR` is this file's directory; `scripts/wiki.py` ships next to it.\nIf nothing resolves, set `WIKI=<absolute path>` manually — never substitute\nyour own layout.",
        "`llm-wiki` is expected on your `PATH` (installed via `install.sh\n--binary` or a release tarball). If it is missing, install it — never\nsubstitute your own layout.",
    ),
    (
        "resolve `$SKILL_DIR/scripts/wiki.py`",
        "- You cannot find `scripts/wiki.py` — resolve `$SKILL_DIR/scripts/wiki.py`\n  first, then the fixed install paths; never substitute your own layout.",
        "- `llm-wiki` is not on your `PATH` — install the binary first; never\n  substitute your own layout.",
    ),
    (
        "goes through\n  `scripts/wiki.py` subcommands",
        "`scripts/wiki.py` subcommands (staged transactions with revision checks)",
        "`llm-wiki` subcommands (staged transactions with revision checks)",
    ),
    (
        "Resolve it once, then run",
        "Resolve it once, then run",
        "Run",
    ),
    // COMMANDS.md preamble.
    (
        "via the `WIKI` variable",
        "Run scripts from the project root via the `WIKI` variable resolved in the\nskill (`python3 \"$WIKI\" <cmd> …`). All commands print one JSON object.",
        "Run commands from the project root; the `llm-wiki` binary must be on\n`PATH` (`llm-wiki <cmd> …`). All commands print one JSON object.",
    ),
    // wiki-visualize: scope the npm dev-loop to repo checkouts — binary
    // installs serve the embedded dist via `llm-wiki viz-serve` (spec §12).
    (
        "The app template\nlives in `$SKILL_DIR/assets/graph-ui/`.",
        "`$SKILL_DIR` is the directory containing this SKILL.md. The app template\nlives in `$SKILL_DIR/assets/graph-ui/`.",
        "This dev flow applies only inside a repo checkout, where the app template\nlives at `skill/wiki-visualize/assets/graph-ui/` (referenced below as\n`$SKILL_DIR`). Binary installs serve the embedded `dist/` — use\n`llm-wiki viz-serve` / `llm-wiki graph-export` below instead.",
    ),
    (
        "If `wiki.py` offers `graph-export`",
        "If `wiki.py` offers `graph-export` / `viz-serve` (SP2b), skip the copy/build\nsteps entirely:",
        "`llm-wiki` offers `graph-export` / `viz-serve` — skip the copy/build\nsteps entirely:",
    ),
];

/// Rewrite one markdown source into the binary variant. Returns the new
/// text; panics when an anchored prose block is missing (drift).
fn rewrite_md(rel: &Path, src: &str) -> String {
    let mut text = src.to_string();
    for (anchor, from, to) in PROSE {
        if text.contains(anchor) {
            assert!(
                text.contains(from),
                "binary-skill rewrite drift: {} contains anchor {:?} but not {:?}",
                rel.display(),
                anchor,
                from
            );
            text = text.replace(from, to);
        }
    }
    // Line-level pass: drop probe lines; rewrite invocations.
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("WIKI=$(ls")
            || t.starts_with("[ -f \"$WIKI\" ]")
            || t.starts_with("# Resolve via")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    let mut text = out;
    text = text.replace("python3 \"$WIKI\"", "llm-wiki");
    // Bare `wiki.py` tokens → `llm-wiki`, but leave `scripts/wiki.py` paths
    // (they name the repo's Python dev tree) alone.
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find("wiki.py") {
        let before = &rest[..i];
        if before.ends_with("scripts/") {
            out.push_str(before);
            out.push_str("wiki.py");
        } else {
            out.push_str(before);
            out.push_str("llm-wiki");
        }
        rest = &rest[i + "wiki.py".len()..];
    }
    out.push_str(rest);
    let text = out;
    assert!(
        !text.contains("\"$WIKI\"") && !text.contains("WIKI=$(ls"),
        "binary-skill rewrite left WIKI residue in {}",
        rel.display()
    );
    text
}

/// Copy `src` into `dst_root/<prefix>` recursively; `scripts/` dirs,
/// `__pycache__`, `*.pyc`, `.DS_Store`, and `node_modules` are excluded.
/// `rewrite` is applied to `.md` files.
fn copy_tree(
    src: &Path,
    dst_root: &Path,
    prefix: &Path,
    rewrite: bool,
    touched: &mut Vec<PathBuf>,
) {
    let mut stack = vec![(src.to_path_buf(), prefix.to_path_buf())];
    while let Some((dir, rel)) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".DS_Store" || name.ends_with(".pyc") {
                continue;
            }
            let ty = entry.file_type().unwrap();
            let child_rel = rel.join(name.as_ref());
            if ty.is_dir() {
                if name == "scripts" || name == "__pycache__" || name == "node_modules" {
                    continue;
                }
                stack.push((entry.path(), child_rel));
            } else if ty.is_file() {
                let dst = dst_root.join(&child_rel);
                fs::create_dir_all(dst.parent().unwrap()).unwrap();
                if rewrite && name.ends_with(".md") {
                    let text = fs::read_to_string(entry.path())
                        .unwrap_or_else(|e| panic!("read {}: {e}", entry.path().display()));
                    fs::write(&dst, rewrite_md(&child_rel, &text)).unwrap();
                } else {
                    fs::copy(entry.path(), &dst)
                        .unwrap_or_else(|e| panic!("copy {}: {e}", entry.path().display()));
                }
                touched.push(child_rel);
            }
            // symlinks: skipped (skill tree is plain files/dirs)
        }
    }
}
fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // manifest = rust/crates/llm-wiki → repo root is three levels up.
    let repo = manifest.join("../../..");
    let skill_src = repo.join("skill");
    assert!(
        skill_src.join("llm-wiki/SKILL.md").is_file(),
        "repo skill tree not found at {}",
        skill_src.display()
    );

    // Rebuild whenever any skill source changes (directory mtimes do not
    // reliably track nested edits — emit one directive per file).
    for entry in Walk::new(&skill_src) {
        println!("cargo:rerun-if-changed={}", entry.display());
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("skill-binary");
    if out.exists() {
        fs::remove_dir_all(&out).unwrap();
    }
    fs::create_dir_all(&out).unwrap();

    // skill/<name>/ → embedded skill/<name>/ minus scripts/ (spec §12).
    let mut touched = Vec::new();
    copy_tree(&skill_src, &out, Path::new("skill"), true, &mut touched);

    // graph-ui/dist/ ← skill/wiki-visualize/assets/graph-ui/dist/ verbatim
    // (the tree viz-serve/graph-export serve from embedded memory).
    let dist = skill_src.join("wiki-visualize/assets/graph-ui/dist");
    if dist.is_dir() {
        copy_tree(&dist, &out, Path::new("graph-ui/dist"), false, &mut touched);
    }

    assert!(
        touched.iter().any(|p| p.ends_with("SKILL.md")),
        "materialized skill tree has no SKILL.md"
    );
}

/// Minimal deterministic recursive file walker (build scripts can't take
/// dependencies beyond std unless declared).
struct Walk {
    stack: Vec<PathBuf>,
}

impl Walk {
    fn new(root: &Path) -> Self {
        Self { stack: vec![root.to_path_buf()] }
    }
}

impl Iterator for Walk {
    type Item = PathBuf;
    fn next(&mut self) -> Option<PathBuf> {
        while let Some(p) = self.stack.pop() {
            let ty = fs::symlink_metadata(&p).ok()?.file_type();
            if ty.is_dir() {
                if let Ok(rd) = fs::read_dir(&p) {
                    for e in rd.flatten() {
                        self.stack.push(e.path());
                    }
                }
            } else if ty.is_file() {
                return Some(p);
            }
        }
        None
    }
}
