//! `mcp-register` (spec §7.8) CLI tests. Every test isolates HOME (and
//! CODEX_HOME for codex) under a tempdir and points PATH at an empty
//! dir so client-CLI probes deterministically miss and the file-edit
//! fallback runs; LLM_WIKI_ROOT is cleared so the project root is cwd.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const START: &str = "<!-- llm-wiki memory:start -->";
const END: &str = "<!-- llm-wiki memory:end -->";

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

/// HOME + project tempdirs plus an isolated empty PATH dir.
struct Env {
    home: TempDir,
    proj: TempDir,
    path: TempDir,
}

fn env() -> Env {
    Env {
        home: tempfile::tempdir().unwrap(),
        proj: tempfile::tempdir().unwrap(),
        path: tempfile::tempdir().unwrap(),
    }
}

/// `mcp-register` invocation under the isolated env; extra args appended.
fn run(e: &Env, args: &[&str]) -> (i32, serde_json::Value) {
    let out = bin()
        .current_dir(e.proj.path())
        .env("HOME", e.home.path())
        .env("CODEX_HOME", e.home.path().join(".codex"))
        .env("PATH", e.path.path())
        .env_remove("LLM_WIKI_ROOT")
        .args(args)
        .output()
        .unwrap();
    let code = out.status.code().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|err| {
            panic!(
                "stdout not JSON ({err}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
    (code, v)
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

fn claude_json(home: &Path) -> PathBuf {
    home.join(".claude.json")
}
fn cursor_json(home: &Path) -> PathBuf {
    home.join(".cursor/mcp.json")
}
fn desktop_json(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Claude/claude_desktop_config.json")
}
fn codex_toml(home: &Path) -> PathBuf {
    home.join(".codex/config.toml")
}

// ---------------------------------------------------------------------------
// registration mechanics
// ---------------------------------------------------------------------------

#[test]
fn cursor_register_creates_config_and_docs() {
    let e = env();
    let cfg = cursor_json(e.home.path());
    let (code, v) = run(&e, &["mcp-register", "--client", "cursor"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["client"], "cursor");
    assert_eq!(v["action"], "registered");
    assert_eq!(v["cli_used"], false);
    assert_eq!(v["config_path"], cfg.to_str().unwrap());
    assert_eq!(v["config_written"], true);
    assert!(v.get("backup").is_none(), "no pre-existing file → no backup");
    assert_eq!(v["docs"]["CLAUDE.md"], "created");
    assert_eq!(v["docs"]["AGENTS.md"], "created");

    let c: serde_json::Value = serde_json::from_str(&read(&cfg)).unwrap();
    assert_eq!(
        c["mcpServers"]["llm-wiki"],
        serde_json::json!({"command": "llm-wiki", "args": ["mcp"]}),
        "bare `llm-wiki mcp`, no --root baked in"
    );
    for doc in ["CLAUDE.md", "AGENTS.md"] {
        let text = read(&e.proj.path().join(doc));
        assert!(text.contains(START) && text.contains(END), "{doc}");
        assert!(text.contains("claim_"), "{doc} cites claim ids");
        assert!(text.contains("wiki_open"), "{doc} discovery clause");
    }
}

#[test]
fn cursor_register_merges_preserving_existing_keys_and_format() {
    let e = env();
    let cfg = cursor_json(e.home.path());
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    // Order + formatting deliberately non-canonical; `other` server must
    // survive byte-for-byte outside our inserted member.
    let orig = "{\n  \"zeta\": 1,\n  \"mcpServers\": {\n    \"other\": {\"command\": \"x\"}\n  }\n}\n";
    std::fs::write(&cfg, orig).unwrap();

    let (code, v) = run(&e, &["mcp-register", "--client", "cursor"]);
    assert_eq!(code, 0, "{v}");
    let bak = v["backup"].as_str().expect("backup path in payload");
    assert_eq!(read(Path::new(bak)), orig, "backup holds pre-edit bytes");

    let merged = read(&cfg);
    let c: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert_eq!(c["zeta"], 1);
    assert_eq!(c["mcpServers"]["other"], serde_json::json!({"command": "x"}));
    assert_eq!(
        c["mcpServers"]["llm-wiki"],
        serde_json::json!({"command": "llm-wiki", "args": ["mcp"]})
    );
    // Key order preserved: zeta still precedes mcpServers.
    assert!(merged.find("\"zeta\"").unwrap() < merged.find("\"mcpServers\"").unwrap());
}

#[test]
fn register_is_idempotent() {
    let e = env();
    let cfg = cursor_json(e.home.path());
    run(&e, &["mcp-register", "--client", "cursor"]);
    let after_first = read(&cfg);
    let (code, v) = run(&e, &["mcp-register", "--client", "cursor"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["config_written"], false, "second run is a no-op write");
    assert_eq!(read(&cfg), after_first);
    assert_eq!(v["docs"]["CLAUDE.md"], "unchanged");
}

#[test]
fn claude_code_falls_back_to_json_when_cli_absent() {
    let e = env();
    let cfg = claude_json(e.home.path());
    let (code, v) = run(&e, &["mcp-register", "--client", "claude-code"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["cli_used"], false, "no claude binary on empty PATH");
    let c: serde_json::Value = serde_json::from_str(&read(&cfg)).unwrap();
    assert_eq!(
        c["mcpServers"]["llm-wiki"]["command"],
        "llm-wiki"
    );
}

#[cfg(unix)]
#[test]
fn claude_code_prefers_client_cli() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    let log = e.proj.path().join("cli.log");
    // Fake `claude` that records its argv and exits 0.
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n", log.display());
    let bin_p = e.path.path().join("claude");
    std::fs::write(&bin_p, script).unwrap();
    std::fs::set_permissions(&bin_p, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (code, v) = run(&e, &["mcp-register", "--client", "claude-code"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["cli_used"], true);
    assert_eq!(v["config_written"], false, "client CLI owns the write");
    assert!(v.get("backup").is_none());
    assert!(!claude_json(e.home.path()).exists(), "fallback did not also write");
    let argv = read(&log);
    assert_eq!(
        argv,
        "mcp\nadd\n--scope\nuser\nllm-wiki\n--\nllm-wiki\nmcp\n",
        "spec §7.8 claude mcp add --scope user llm-wiki -- llm-wiki mcp"
    );
    // Docs block still lands even on the CLI path.
    assert_eq!(v["docs"]["CLAUDE.md"], "created");
}

#[test]
fn claude_desktop_registers_at_platform_path() {
    let e = env();
    let cfg = desktop_json(e.home.path());
    let (code, v) = run(&e, &["mcp-register", "--client", "claude-desktop"]);
    assert_eq!(code, 0, "{v}");
    assert!(cfg.exists(), "parent dirs created under temp HOME");
    let c: serde_json::Value = serde_json::from_str(&read(&cfg)).unwrap();
    assert_eq!(c["mcpServers"]["llm-wiki"]["args"][0], "mcp");
}

#[test]
fn codex_falls_back_to_toml() {
    let e = env();
    let cfg = codex_toml(e.home.path());
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let orig = "[other]\nx = 1\n";
    std::fs::write(&cfg, orig).unwrap();

    let (code, v) = run(&e, &["mcp-register", "--client", "codex"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["cli_used"], false);
    let text = read(&cfg);
    assert!(text.starts_with(orig), "existing content untouched");
    assert!(
        text.contains("[mcp_servers.llm-wiki]"),
        "TOML table, NOT JSON mcpServers: {text}"
    );
    assert!(text.contains("command = \"llm-wiki\""), "{text}");
    assert!(text.contains("args = [\"mcp\"]"), "{text}");
}

#[test]
fn codex_toml_updates_existing_table() {
    let e = env();
    let cfg = codex_toml(e.home.path());
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let orig = "# user comment\n[mcp_servers.llm-wiki]\ncommand = \"old-bin\" # keep\nargs = [\"x\"]\n";
    std::fs::write(&cfg, orig).unwrap();
    let (code, _) = run(&e, &["mcp-register", "--client", "codex"]);
    assert_eq!(code, 0);
    let text = read(&cfg);
    assert_eq!(text.matches("[mcp_servers.llm-wiki]").count(), 1);
    assert!(text.contains("command = \"llm-wiki\" # keep"), "comment survives: {text}");
    assert!(text.contains("args = [\"mcp\"]"), "{text}");
    assert!(text.starts_with("# user comment\n"));
}

#[test]
fn codex_cli_used_when_binary_succeeds() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    let log = e.proj.path().join("cli.log");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n", log.display());
    let bin_p = e.path.path().join("codex");
    std::fs::write(&bin_p, script).unwrap();
    std::fs::set_permissions(&bin_p, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (code, v) = run(&e, &["mcp-register", "--client", "codex"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["cli_used"], true);
    assert!(!codex_toml(e.home.path()).exists());
    assert_eq!(read(&log), "mcp\nadd\nllm-wiki\n--\nllm-wiki\nmcp\n");
}

// ---------------------------------------------------------------------------
// managed CLAUDE.md / AGENTS.md block
// ---------------------------------------------------------------------------

#[test]
fn doc_block_replaces_in_place_between_markers() {
    let e = env();
    let claude = e.proj.path().join("CLAUDE.md");
    let orig = format!("# notes\n{START}\nold stale block\n{END}\ntrailer\n");
    std::fs::write(&claude, &orig).unwrap();
    let (code, v) = run(&e, &["mcp-register", "--client", "cursor"]);
    assert_eq!(code, 0, "{v}");
    let text = read(&claude);
    assert!(text.starts_with("# notes\n"), "content above preserved");
    assert!(text.ends_with("trailer\n"), "content below preserved");
    assert!(!text.contains("old stale block"));
    assert_eq!(v["docs"]["CLAUDE.md"], "updated");
}

#[test]
fn doc_block_appends_to_existing_file() {
    let e = env();
    let agents = e.proj.path().join("AGENTS.md");
    std::fs::write(&agents, "# agents\n").unwrap();
    run(&e, &["mcp-register", "--client", "cursor"]);
    let text = read(&agents);
    assert!(text.starts_with("# agents\n"));
    assert_eq!(text.matches(START).count(), 1);
}

// ---------------------------------------------------------------------------
// uninstall + byte-identical round-trips
// ---------------------------------------------------------------------------

#[test]
fn uninstall_removes_entry_block_and_cleans_up_created_files() {
    let e = env();
    run(&e, &["mcp-register", "--client", "cursor"]);
    let (code, v) = run(&e, &["mcp-register", "--client", "cursor", "--uninstall"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["action"], "unregistered");
    assert_eq!(v["entry_removed"], true);
    assert_eq!(v["config_deleted"], true, "file register created goes away");
    assert!(!cursor_json(e.home.path()).exists());
    for doc in ["CLAUDE.md", "AGENTS.md"] {
        assert!(!e.proj.path().join(doc).exists(), "{doc} removed");
        assert_eq!(v["docs"][doc], "removed");
    }
}

#[test]
fn uninstall_restores_preexisting_config_byte_identical() {
    let e = env();
    let cfg = cursor_json(e.home.path());
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let orig = "{\"mcpServers\":{}}";
    std::fs::write(&cfg, orig).unwrap(); // ambiguous empty-object case
    run(&e, &["mcp-register", "--client", "cursor"]);
    let (code, v) = run(&e, &["mcp-register", "--client", "cursor", "--uninstall"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["entry_removed"], true);
    assert_eq!(read(&cfg), orig, "empty mcpServers shell restored exactly");
}

#[test]
fn uninstall_preserves_other_servers_and_doc_content() {
    let e = env();
    let cfg = claude_json(e.home.path());
    let orig = "{\n  \"mcpServers\": {\n    \"a\": {},\n    \"b\": {}\n  }\n}";
    std::fs::write(&cfg, orig).unwrap();
    let claude = e.proj.path().join("CLAUDE.md");
    std::fs::write(&claude, "# mine\n").unwrap();

    run(&e, &["mcp-register", "--client", "claude-code"]);
    let merged = read(&cfg);
    assert!(merged.contains("\"llm-wiki\""));
    assert!(merged.contains("\"a\"") && merged.contains("\"b\""));

    let (code, v) = run(&e, &["mcp-register", "--client", "claude-code", "--uninstall"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(read(&cfg), orig, "surgical restore byte-identical");
    assert_eq!(read(&claude), "# mine\n", "user doc content untouched");
}

/// register→uninstall leaves each client's config byte-identical to its
/// pre-register bytes across representative pre-states.
#[test]
fn roundtrip_byte_identical_all_clients() {
    let clients: &[(&str, fn(&Path) -> PathBuf, &[&str])] = &[
        (
            "claude-code",
            claude_json,
            &[
                "",
                "{\"mcpServers\":{}}",
                "{\"x\":1,\"mcpServers\":{\"a\":{}}}",
                "{\n \"z\": 1\n}\n",
            ],
        ),
        (
            "cursor",
            cursor_json,
            &["", "{\"mcpServers\":{\"a\":{\"command\":\"c\"}}}"],
        ),
        ("claude-desktop", desktop_json, &["", "{}"]),
        (
            "codex",
            codex_toml,
            &[
                "",
                "[other]\nx = 1\n",
                "[other]\nx = 1", // no trailing newline (marker path)
                "# c\n\n[t]\nk = 2\n",
            ],
        ),
    ];
    for (client, cfg_fn, presets) in clients {
        for pre in *presets {
            let e = env();
            let cfg = cfg_fn(e.home.path());
            let doc_before = e.proj.path().join("CLAUDE.md");
            std::fs::write(&doc_before, "user\n").unwrap();
            if !pre.is_empty() {
                std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
                std::fs::write(&cfg, pre).unwrap();
            }
            let existed = !pre.is_empty();

            let (c1, v1) = run(&e, &["mcp-register", "--client", client]);
            assert_eq!(c1, 0, "{client} register: {v1}");
            let (c2, v2) = run(&e, &["mcp-register", "--client", client, "--uninstall"]);
            assert_eq!(c2, 0, "{client} uninstall: {v2}");

            if existed {
                assert_eq!(
                    read(&cfg),
                    *pre,
                    "{client} config not byte-identical after round-trip (pre={pre:?})"
                );
            } else {
                assert!(
                    !cfg.exists(),
                    "{client} config should be removed after round-trip"
                );
            }
            assert_eq!(read(&doc_before), "user\n", "{client} CLAUDE.md round-trip");
        }
    }
}

#[test]
fn uninstall_when_nothing_registered_is_noop() {
    let e = env();
    let cfg = cursor_json(e.home.path());
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let orig = "{\"mcpServers\":{\"a\":{}}}";
    std::fs::write(&cfg, orig).unwrap();
    let (code, v) = run(&e, &["mcp-register", "--client", "cursor", "--uninstall"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["entry_removed"], false);
    assert_eq!(v["config_written"], false);
    assert_eq!(read(&cfg), orig);
    assert_eq!(v["docs"]["CLAUDE.md"], "absent");
}

#[test]
fn usage_error_on_missing_client() {
    let e = env();
    let out = bin()
        .current_dir(e.proj.path())
        .env("HOME", e.home.path())
        .env("PATH", e.path.path())
        .env_remove("LLM_WIKI_ROOT")
        .args(["mcp-register"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error_type"], "usage");
}
