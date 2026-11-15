//! `--install-skills` top-level flag (spec §12): copy-mode install of the
//! embedded *binary variant* skill set — `skill/<name>/` materialized as
//! `<target>/<name>/`, `scripts/` omitted, SKILL.md/references rewritten to
//! `llm-wiki <cmd>` invocations. Refresh = remove-and-replace managed
//! files, preserve unmanaged. `--agents`/`--cellockai` presets are mutually
//! exclusive with the positional DIR and each other.

use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn run_json(cmd: &mut Command) -> (i32, Value) {
    let out = cmd.output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("stdout not JSON ({e}): {:?}", String::from_utf8_lossy(&out.stdout))
    });
    (code, v)
}

const EXPECTED: [&str; 10] = [
    "llm-wiki",
    "wiki-compile",
    "wiki-context",
    "wiki-doctor",
    "wiki-ingest",
    "wiki-init",
    "wiki-query",
    "wiki-review",
    "wiki-visualize",
    "wiki-write",
];

fn assert_installed(target: &std::path::Path) {
    for name in EXPECTED {
        let dir = target.join(name);
        assert!(dir.join("SKILL.md").is_file(), "missing {name}/SKILL.md");
        assert!(dir.join(".llm-wiki-installed").is_file(), "missing {name} marker");
    }
}

#[test]
fn install_skills_materializes_binary_variant() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("skills");
    let (code, v) = run_json(
        bin().args(["--install-skills", target.to_str().unwrap()]),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["count"], 10);
    assert_eq!(v["target"], target.to_string_lossy().to_string());
    let names: Vec<String> = v["installed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, EXPECTED);
    assert_installed(&target);
    // The top-level graph-ui/dist embed is for in-binary viz serving, not
    // installed into the skills dir.
    assert!(!target.join("graph-ui").exists());

    // No scripts/ dirs or Python entrypoints anywhere in the install.
    for entry in walk(&target) {
        let s = entry.to_string_lossy();
        assert!(!s.contains("/scripts"), "scripts/ leaked: {s}");
        assert!(!s.ends_with(".pyc"), "pyc leaked: {s}");
    }

    // Binary variant text: llm-wiki on PATH, no python3 "$WIKI", no probes.
    for name in EXPECTED {
        let skill = std::fs::read_to_string(target.join(name).join("SKILL.md")).unwrap();
        assert!(!skill.contains("\"$WIKI\""), "{name}: WIKI residue");
        assert!(!skill.contains("WIKI=$(ls"), "{name}: probe residue");
        assert!(
            !skill.contains("wiki.py") || name == "llm-wiki" && {
                // llm-wiki SKILL.md may reference scripts/wiki.py repo paths —
                // allowed only inside a `scripts/wiki.py` path token.
                skill
                    .split_whitespace()
                    .filter(|t| t.contains("wiki.py"))
                    .all(|t| t.contains("scripts/wiki.py"))
            },
            "{name}: bare wiki.py token"
        );
    }
    // Invocation examples rewritten.
    let compile =
        std::fs::read_to_string(target.join("wiki-compile/SKILL.md")).unwrap();
    assert!(compile.contains("llm-wiki compile"), "{compile}");
    let commands = std::fs::read_to_string(
        target.join("llm-wiki/references/COMMANDS.md"),
    )
    .unwrap();
    assert!(commands.contains("llm-wiki init"), "{commands}");
    assert!(!commands.contains("wiki.py "), "{commands}");
    // wiki-visualize dev-loop is kept but scoped to repo checkouts.
    let viz =
        std::fs::read_to_string(target.join("wiki-visualize/SKILL.md")).unwrap();
    assert!(viz.contains("repo checkout"), "{viz}");
    assert!(viz.contains("llm-wiki viz-serve"), "{viz}");
}

#[test]
fn install_skills_refresh_preserves_unmanaged() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("skills");
    let (code, v) = run_json(
        bin().args(["--install-skills", target.to_str().unwrap()]),
    );
    assert_eq!(code, 0, "{v}");

    // Rogue (unmanaged) file inside a skill dir + a mutated managed file.
    let rogue = target.join("wiki-query/notes-from-user.md");
    std::fs::write(&rogue, "user content").unwrap();
    let managed = target.join("wiki-query/SKILL.md");
    std::fs::write(&managed, "clobbered").unwrap();

    let (code, v) = run_json(
        bin().args(["--install-skills", target.to_str().unwrap()]),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(std::fs::read_to_string(&rogue).unwrap(), "user content");
    let restored = std::fs::read_to_string(&managed).unwrap();
    assert!(restored.contains("llm-wiki query-prepare"), "{restored}");
    assert_ne!(restored, "clobbered");
}

#[test]
fn install_skills_presets_and_conflicts() {
    let home = tempfile::tempdir().unwrap();

    // --agents preset → $HOME/.agents/skills
    let (code, v) = run_json(
        bin()
            .env("HOME", home.path())
            .args(["--install-skills", "--agents"]),
    );
    assert_eq!(code, 0, "{v}");
    let agents = home.path().join(".agents/skills");
    assert_eq!(v["target"], agents.to_string_lossy().to_string());
    assert_installed(&agents);

    // --cellockai preset → $HOME/.cellockai/skills
    let (code, v) = run_json(
        bin()
            .env("HOME", home.path())
            .args(["--install-skills", "--cellockai"]),
    );
    assert_eq!(code, 0, "{v}");
    assert_installed(&home.path().join(".cellockai/skills"));

    // bare --install-skills → $HOME/.omp/agent/skills
    let (code, v) = run_json(
        bin().env("HOME", home.path()).args(["--install-skills"]),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v["target"],
        home.path()
            .join(".omp/agent/skills")
            .to_string_lossy()
            .to_string()
    );

    // DIR + preset → usage error, JSON, exit 2.
    let tmp = tempfile::tempdir().unwrap();
    let (code, v) = run_json(bin().args([
        "--install-skills",
        tmp.path().join("x").to_str().unwrap(),
        "--agents",
    ]));
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error_type"], "usage");

    // --agents + --cellockai → clap conflict, exit 2.
    let (code, v) = run_json(bin().args(["--install-skills", "--agents", "--cellockai"]));
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error_type"], "usage");

    // preset without --install-skills → required-arg failure, exit 2.
    let (code, v) = run_json(bin().args(["--agents"]));
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error_type"], "usage");

    // flag after a subcommand is rejected (it is not a global arg).
    let (code, v) = run_json(bin().args(["status", "--install-skills"]));
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error_type"], "usage");
}

/// Recursive file/dir listing under `root` (files and dirs).
fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
            out.push(e.path());
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(e.path());
            }
        }
    }
    out
}
