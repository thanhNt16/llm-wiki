//! `mcp-register` (spec §7.8): register/unregister the bare `llm-wiki mcp`
//! MCP server in agent-client configs, plus the CLAUDE.md/AGENTS.md
//! managed memory block.
//!
//! Edits are **surgical text edits**, never a parse→serialize round-trip:
//! user formatting, comments (TOML) and key order are preserved exactly,
//! and a register→uninstall round-trip leaves a pre-existing config file
//! byte-identical. Byte-identity is guaranteed two ways, checked in order:
//!
//! 1. **Backup restore.** The newest `<config>.bak-<ts>` backup holds the
//!    pre-register bytes; if the current file still equals what
//!    `register` would have produced from those bytes (i.e. nothing else
//!    touched it), uninstall restores the backup verbatim.
//! 2. **Surgical removal.** Otherwise only the member/table `register`
//!    adds is excised — deterministically reversible because register
//!    always appends a new member with a leading `,` separator and removes
//!    it with that same separator.
//!
//! A config that did not exist before registration is deleted on
//! uninstall (recognized by its exact canonical bytes).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::util;

/// Server key registered under `mcpServers` / `[mcp_servers.*]` — bare
/// command, no `--root` baked in (spec §7.8: global registrations spawn
/// the server from arbitrary directories).
pub const SERVER_NAME: &str = "llm-wiki";
const ENTRY_JSON: &str = "{\"command\":\"llm-wiki\",\"args\":[\"mcp\"]}";
const MEMBER_JSON: &str = "\"llm-wiki\":{\"command\":\"llm-wiki\",\"args\":[\"mcp\"]}";
const MCP_SERVERS_MEMBER: &str =
    "\"mcpServers\":{\"llm-wiki\":{\"command\":\"llm-wiki\",\"args\":[\"mcp\"]}}";

/// Canonical bytes for a config file created from scratch by register —
/// recognized on uninstall so the file can be deleted back out.
const CREATED_JSON: &str =
    "{\"mcpServers\":{\"llm-wiki\":{\"command\":\"llm-wiki\",\"args\":[\"mcp\"]}}}\n";
const CREATED_TOML: &str = "[mcp_servers.llm-wiki]\ncommand = \"llm-wiki\"\nargs = [\"mcp\"]\n";
/// Marker comment used when a TOML append had to synthesize a newline
/// onto a file that did not end with one — distinguishes that newline
/// from one the user already had so uninstall can restore either.
const TOML_NL_MARKER: &str = "# added by llm-wiki mcp-register";

const BLOCK_START: &str = "<!-- llm-wiki memory:start -->";
const BLOCK_END: &str = "<!-- llm-wiki memory:end -->";
const MEMORY_BLOCK: &str = "<!-- llm-wiki memory:start -->\n\
# llm-wiki memory\n\
\n\
This project keeps a deterministic, file-based memory store at `.llm-wiki/`.\n\
\n\
When answering questions about this project:\n\
- Prefer wiki facts over guessing; cite claim ids (e.g. `claim_...`) for \
every fact taken from the wiki.\n\
- Surface conflicts and gaps explicitly — never smooth them over.\n\
- If no wiki session is open, call the `wiki_open` MCP tool first \
(no `--root` is baked into the registered server).\n\
<!-- llm-wiki memory:end -->\n";

const DOC_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// Agent clients understood by `--client` (clap renders these
/// kebab-case: `claude-code`, `cursor`, `claude-desktop`, `codex`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum McpClient {
    /// `claude mcp add --scope user`; fallback JSON edit of ~/.claude.json.
    ClaudeCode,
    /// JSON edit of ~/.cursor/mcp.json.
    Cursor,
    /// JSON edit of the platform claude_desktop_config.json.
    ClaudeDesktop,
    /// `codex mcp add`; fallback TOML edit of config.toml.
    Codex,
}

impl McpClient {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Codex => "codex",
        }
    }
}

fn home_dir() -> Result<PathBuf, EngineError> {
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    #[cfg(windows)]
    if let Some(h) = std::env::var_os("USERPROFILE") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    Err(EngineError::Txn(
        "could not resolve home directory (HOME unset)".into(),
    ))
}

fn config_path(client: McpClient, home: &Path) -> Result<PathBuf, EngineError> {
    Ok(match client {
        McpClient::ClaudeCode => home.join(".claude.json"),
        McpClient::Cursor => home.join(".cursor").join("mcp.json"),
        McpClient::ClaudeDesktop => {
            #[cfg(target_os = "macos")]
            {
                home.join("Library")
                    .join("Application Support")
                    .join("Claude")
                    .join("claude_desktop_config.json")
            }
            #[cfg(target_os = "windows")]
            {
                let appdata = std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join("AppData").join("Roaming"));
                appdata.join("Claude").join("claude_desktop_config.json")
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                home.join(".config")
                    .join("Claude")
                    .join("claude_desktop_config.json")
            }
        }
        McpClient::Codex => {
            let codex_home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            codex_home.join("config.toml")
        }
    })
}

/// Client-CLI registration path (spec §7.8): probe by running the
/// command; a nonzero exit or an absent binary falls back to the file
/// edit. Output is captured so the client CLI cannot pollute the
/// one-JSON-object stdout contract (§6.2).
fn cli_args(client: McpClient, uninstall: bool) -> Option<(&'static str, Vec<&'static str>)> {
    match (client, uninstall) {
        (McpClient::ClaudeCode, false) => Some((
            "claude",
            vec![
                "mcp", "add", "--scope", "user", SERVER_NAME, "--", "llm-wiki", "mcp",
            ],
        )),
        (McpClient::ClaudeCode, true) => {
            Some(("claude", vec!["mcp", "remove", SERVER_NAME, "--scope", "user"]))
        }
        (McpClient::Codex, false) => Some((
            "codex",
            vec!["mcp", "add", SERVER_NAME, "--", "llm-wiki", "mcp"],
        )),
        (McpClient::Codex, true) => Some(("codex", vec!["mcp", "remove", SERVER_NAME])),
        _ => None,
    }
}

fn try_client_cli(client: McpClient, uninstall: bool) -> bool {
    let Some((prog, args)) = cli_args(client, uninstall) else {
        return false;
    };
    Command::new(prog)
        .args(&args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Minimal JSON scanner — byte-accurate member spans. Input is always
// pre-validated by serde_json, so structural errors are impossible in
// practice; they still surface as Txn errors rather than panics.
// ---------------------------------------------------------------------------

struct Member {
    key: String,
    key_start: usize,
    value_start: usize,
    /// one past the value's last byte
    end: usize,
}

fn bad_json(what: &str) -> EngineError {
    EngineError::Txn(format!("mcp-register: config is not editable JSON ({what})"))
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// `b[i] == '"'` → one past the closing quote.
fn skip_string(b: &[u8], mut i: usize) -> Result<usize, EngineError> {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err(bad_json("unterminated string"))
}

/// `i` at a value's first byte → one past its last byte.
fn skip_value(b: &[u8], i: usize) -> Result<usize, EngineError> {
    match b.get(i) {
        Some(b'"') => skip_string(b, i),
        Some(b'{') | Some(b'[') => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => j = skip_string(b, j)?,
                    b'{' | b'[' => {
                        depth += 1;
                        j += 1;
                    }
                    b'}' | b']' => {
                        depth -= 1;
                        j += 1;
                        if depth == 0 {
                            return Ok(j);
                        }
                    }
                    _ => j += 1,
                }
            }
            Err(bad_json("unterminated object/array"))
        }
        Some(_) => {
            let mut j = i;
            while j < b.len()
                && !matches!(b[j], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            {
                j += 1;
            }
            Ok(j)
        }
        None => Err(bad_json("missing value")),
    }
}

/// Members of the object whose `{` is at `open`; also yields the index of
/// the closing `}`.
fn object_members(text: &str, open: usize) -> Result<(Vec<Member>, usize), EngineError> {
    let b = text.as_bytes();
    if b.get(open) != Some(&b'{') {
        return Err(bad_json("expected '{'"));
    }
    let mut members = Vec::new();
    let mut i = skip_ws(b, open + 1);
    loop {
        match b.get(i) {
            Some(b'}') => return Ok((members, i)),
            Some(b'"') => {
                let key_start = i;
                let key_end = skip_string(b, i)?;
                let key: String =
                    serde_json::from_str(&text[key_start..key_end]).map_err(EngineError::from)?;
                let j = skip_ws(b, key_end);
                if b.get(j) != Some(&b':') {
                    return Err(bad_json("expected ':'"));
                }
                let value_start = skip_ws(b, j + 1);
                let end = skip_value(b, value_start)?;
                members.push(Member {
                    key,
                    key_start,
                    value_start,
                    end,
                });
                i = skip_ws(b, end);
                match b.get(i) {
                    Some(b',') => i = skip_ws(b, i + 1),
                    Some(b'}') => return Ok((members, i)),
                    _ => return Err(bad_json("expected ',' or '}'")),
                }
            }
            _ => return Err(bad_json("expected member")),
        }
    }
}

/// Locate the root object's `{`…`}` span (whole file must be one object).
fn root_object(text: &str) -> Result<(usize, usize), EngineError> {
    let b = text.as_bytes();
    let open = skip_ws(b, 0);
    if b.get(open) != Some(&b'{') {
        return Err(EngineError::Txn(
            "mcp-register: config root is not a JSON object".into(),
        ));
    }
    let end = skip_value(b, open)?;
    if skip_ws(b, end) != b.len() {
        return Err(bad_json("trailing bytes after root object"));
    }
    Ok((open, end - 1))
}

/// Insert `member` (`"key":value`) as the LAST member of the object
/// spanning `open..=close`. Leading `,` when the object is non-empty so
/// the inserted bytes are exactly what `remove_member` excises.
fn insert_member(text: &str, open: usize, close: usize, member: &str) -> String {
    let empty = skip_ws(text.as_bytes(), open + 1) == close;
    if empty {
        format!("{}{}{}", &text[..open + 1], member, &text[close..])
    } else {
        format!("{},{}{}", &text[..close], member, &text[close..])
    }
}

/// Excise `m` plus whichever separator accompanies it: the `,` before it
/// when it is a non-first member (the one `insert_member` adds), else the
/// `,` after it; a sole member loses neither.
fn remove_member(text: &str, m: &Member) -> String {
    let b = text.as_bytes();
    let mut before = m.key_start;
    while before > 0 && matches!(b[before - 1], b' ' | b'\t' | b'\n' | b'\r') {
        before -= 1;
    }
    if before > 0 && b[before - 1] == b',' {
        format!("{}{}", &text[..before - 1], &text[m.end..])
    } else {
        let mut after = skip_ws(b, m.end);
        if b.get(after) == Some(&b',') {
            after += 1;
            format!("{}{}", &text[..m.key_start], &text[after..])
        } else {
            format!("{}{}", &text[..m.key_start], &text[m.end..])
        }
    }
}

/// Register `mcpServers["llm-wiki"] = {command,args}` in `text`
/// (pre-validated JSON object). Idempotent: returns `text` unchanged when
/// the entry is already ours.
fn register_json(text: &str) -> Result<String, EngineError> {
    let (open, close) = root_object(text)?;
    let (members, _) = object_members(text, open)?;
    match members.iter().find(|m| m.key == "mcpServers") {
        None => Ok(insert_member(text, open, close, MCP_SERVERS_MEMBER)),
        Some(ms) => {
            if text.as_bytes().get(ms.value_start) != Some(&b'{') {
                return Err(EngineError::Txn(
                    "mcp-register: \"mcpServers\" is not a JSON object".into(),
                ));
            }
            let (inner, ms_close) = object_members(text, ms.value_start)?;
            match inner.iter().find(|m| m.key == SERVER_NAME) {
                Some(lw) => Ok(format!(
                    "{}{}{}",
                    &text[..lw.value_start],
                    ENTRY_JSON,
                    &text[lw.end..]
                )),
                None => Ok(insert_member(text, ms.value_start, ms_close, MEMBER_JSON)),
            }
        }
    }
}

/// Surgical uninstall from `text` (pre-validated JSON object). Returns
/// `(new_text, entry_removed)`. An `mcpServers` left holding no members
/// is excised too — an empty server map is only ever the shell our
/// registration left behind.
fn unregister_json(text: &str) -> Result<(String, bool), EngineError> {
    let (open, _) = root_object(text)?;
    let (members, _) = object_members(text, open)?;
    let Some(ms) = members.iter().find(|m| m.key == "mcpServers") else {
        return Ok((text.to_string(), false));
    };
    if text.as_bytes().get(ms.value_start) != Some(&b'{') {
        return Err(EngineError::Txn(
            "mcp-register: \"mcpServers\" is not a JSON object".into(),
        ));
    }
    let (inner, _) = object_members(text, ms.value_start)?;
    if !inner.iter().any(|m| m.key == SERVER_NAME) {
        return Ok((text.to_string(), false));
    }
    if inner.len() == 1 {
        Ok((remove_member(text, ms), true))
    } else {
        let lw = inner.iter().find(|m| m.key == SERVER_NAME).unwrap();
        Ok((remove_member(text, lw), true))
    }
}

// ---------------------------------------------------------------------------
// TOML — line-level edits. `[mcp_servers.llm-wiki]` is the contractual
// table (spec §7.8; codex uses TOML, NOT the JSON `mcpServers` shape). A
// `[mcp_servers]` inline `llm-wiki = {...}` key is honored too so an
// appended table can never duplicate a key.
// ---------------------------------------------------------------------------

struct TomlTable {
    /// byte offset of the header line's first char
    header_start: usize,
    /// byte offset of the next table header, or text.len()
    end: usize,
    /// normalized dotted path, e.g. ["mcp_servers", "llm-wiki"]
    path: Vec<String>,
}

fn toml_tables(text: &str) -> Vec<TomlTable> {
    let mut headers = Vec::new();
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with('[') && !t.starts_with("[[") && t.ends_with(']') {
            let inner = &t[1..t.len() - 1];
            let path: Vec<String> = inner
                .split('.')
                .map(|s| {
                    let s = s.trim();
                    s.trim_matches('"').trim_matches('\'').to_string()
                })
                .collect();
            headers.push((pos, path));
        }
        pos += line.len();
    }
    headers
        .iter()
        .enumerate()
        .map(|(i, &(start, ref path))| TomlTable {
            header_start: start,
            end: headers.get(i + 1).map(|h| h.0).unwrap_or(text.len()),
            path: path.clone(),
        })
        .collect()
}

/// First line inside `text[start..end)` whose key is `key`; yields the
/// line's byte range (excluding newline) and the index of `=` within it.
fn toml_key_line(text: &str, start: usize, end: usize, key: &str) -> Option<(usize, usize)> {
    let mut pos = start;
    while pos < end {
        let nl = text[pos..end].find('\n').map(|o| pos + o).unwrap_or(end);
        let line = &text[pos..nl];
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix(key) {
            if rest.starts_with('=') || rest.starts_with(char::is_whitespace) {
                // `command`/`args` — not `commands`, `command.x`, …
                let rest = rest.trim_start();
                if rest.starts_with('=') {
                    return Some((pos + (line.len() - t.len()), nl));
                }
            }
        }
        pos = nl + 1;
    }
    None
}

fn toml_table_path(target: &[&str]) -> Vec<String> {
    target.iter().map(|s| s.to_string()).collect()
}

/// Byte span of the value on a TOML `key = value` line `[ls, le)`
/// (line's `=` at or after `ls`): first non-ws byte after `=` through
/// the last byte before a top-level `#` comment or EOL, ws-trimmed.
/// Strings are scanned so `#`/`"` inside them never truncate.
fn toml_value_span(text: &str, ls: usize, le: usize) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let eq = text[ls..le].find('=')? + ls;
    let mut vs = eq + 1;
    while vs < le && matches!(b[vs], b' ' | b'\t') {
        vs += 1;
    }
    let mut ve = le;
    let (mut in_sq, mut in_dq) = (false, false);
    let mut i = vs;
    while i < le {
        match b[i] {
            b'\'' if !in_dq => in_sq = !in_sq,
            b'"' if !in_sq => in_dq = !in_dq,
            b'\\' if in_dq => i += 1,
            b'#' if !in_sq && !in_dq => {
                ve = i;
                break;
            }
            _ => {}
        }
        i += 1;
    }
    while ve > vs && matches!(b[ve - 1], b' ' | b'\t') {
        ve -= 1;
    }
    Some((vs, ve))
}

fn register_toml(text: &str) -> Result<String, EngineError> {
    let tables = toml_tables(text);
    let target = toml_table_path(&["mcp_servers", SERVER_NAME]);
    if let Some(t) = tables.iter().find(|t| t.path == target) {
        // Existing table: upsert command/args in place — replace only
        // the value token so indentation and trailing comments survive.
        let header_nl = text[t.header_start..t.end]
            .find('\n')
            .map(|o| t.header_start + o + 1)
            .unwrap_or(t.end);
        let mut out = text[..t.end].to_string();
        let tail = text[t.end..].to_string();
        let mut missing: Vec<&str> = Vec::new();
        for (key, val) in [("command", "\"llm-wiki\""), ("args", "[\"mcp\"]")] {
            match toml_key_line(&out, header_nl, out.len(), key) {
                Some((ls, le)) => {
                    let (vs, ve) = toml_value_span(&out, ls, le)
                        .ok_or_else(|| bad_json("TOML key line missing '='"))?;
                    out = format!("{}{}{}", &out[..vs], val, &out[ve..]);
                }
                None => missing.push(match key {
                    "command" => "command = \"llm-wiki\"\n",
                    _ => "args = [\"mcp\"]\n",
                }),
            }
        }
        if !missing.is_empty() {
            // Missing keys go on fresh lines at the table's end.
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for line in missing {
                out.push_str(line);
            }
        }
        Ok(format!("{}{}", out, tail))
    } else if let Some(t) = tables
        .iter()
        .find(|t| t.path == toml_table_path(&["mcp_servers"]))
    {
        // `[mcp_servers]` with an inline `llm-wiki = {...}` key would
        // collide with an appended table — upsert the inline key.
        let header_nl = text[t.header_start..t.end]
            .find('\n')
            .map(|o| t.header_start + o + 1)
            .unwrap_or(t.end);
        if let Some((ls, le)) = toml_key_line(text, header_nl, t.end, SERVER_NAME) {
            let (vs, ve) = toml_value_span(text, ls, le)
                .ok_or_else(|| bad_json("TOML key line missing '='"))?;
            return Ok(format!(
                "{}{}{}",
                &text[..vs],
                "{ command = \"llm-wiki\", args = [\"mcp\"] }",
                &text[ve..]
            ));
        }
        // No inline key: a sub-table append stays valid TOML.
        Ok(append_toml_table(text))
    } else {
        Ok(append_toml_table(text))
    }
}

/// Append the `[mcp_servers.llm-wiki]` block at EOF. The
/// `TOML_NL_MARKER` comment is prepended only when the file does not end
/// with a newline — it marks the synthesized newline so uninstall knows
/// whether to hand it back.
fn append_toml_table(text: &str) -> String {
    if text.is_empty() {
        return CREATED_TOML.to_string();
    }
    if text.ends_with('\n') {
        format!("{}{}", text, CREATED_TOML)
    } else {
        format!("{}\n{}\n{}", text, TOML_NL_MARKER, CREATED_TOML)
    }
}

/// Surgical TOML uninstall. `(new_text, entry_removed)`. Whole-region
/// removal: the `[mcp_servers.llm-wiki]` table (or the inline
/// `llm-wiki = {...}` key under `[mcp_servers]`) is the registration.
///
/// EOF separator rule: appends add the table directly after the file's
/// last byte — no separator when the file ended `\n`-terminated, one
/// synthesized `\n` + `TOML_NL_MARKER` when it did not. Removal at EOF
/// therefore consumes one preceding `\n` ONLY for the marker case.
fn unregister_toml(text: &str) -> Result<(String, bool), EngineError> {
    let tables = toml_tables(text);
    let target = toml_table_path(&["mcp_servers", SERVER_NAME]);
    if let Some(t) = tables.iter().find(|t| t.path == target) {
        let mut start = t.header_start;
        let mut marker_folded = false;
        if start > 0 && text.as_bytes()[start - 1] == b'\n' {
            let prev_start = text[..start - 1].rfind('\n').map(|o| o + 1).unwrap_or(0);
            if text[prev_start..start - 1].trim() == TOML_NL_MARKER {
                start = prev_start;
                marker_folded = true;
            }
        }
        let mut remove_start = start;
        if marker_folded && t.end == text.len() && start > 0 {
            remove_start = start - 1; // synthesized `\n` before the marker
        }
        Ok((
            format!("{}{}", &text[..remove_start], &text[t.end..]),
            true,
        ))
    } else if let Some(t) = tables
        .iter()
        .find(|t| t.path == toml_table_path(&["mcp_servers"]))
    {
        let header_nl = text[t.header_start..t.end]
            .find('\n')
            .map(|o| t.header_start + o + 1)
            .unwrap_or(t.end);
        if let Some((ls, le)) = toml_key_line(text, header_nl, t.end, SERVER_NAME) {
            // Remove the key's whole line with its trailing newline.
            // Inline keys are user-authored (register only ever appends
            // a sub-table), so no separator is handed back.
            let mut re = le;
            if re < text.len() && text.as_bytes()[re] == b'\n' {
                re += 1;
            }
            return Ok((format!("{}{}", &text[..ls], &text[re..]), true));
        }
        Ok((text.to_string(), false))
    } else {
        Ok((text.to_string(), false))
    }
}

// ---------------------------------------------------------------------------
// File plumbing: backup → tmp+rename; CLAUDE.md/AGENTS.md managed block.
// ---------------------------------------------------------------------------

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `<config>.bak-<ts>` (`-<n>` suffix on same-second collisions); returns
/// the backup path, or None when `path` did not exist.
fn backup_file(path: &Path) -> Result<Option<PathBuf>, EngineError> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = unix_ts();
    for n in 0u32..100 {
        let name = if n == 0 {
            format!("{}.bak-{}", path.display(), ts)
        } else {
            format!("{}.bak-{}-{}", path.display(), ts, n)
        };
        let bak = PathBuf::from(name);
        if !bak.exists() {
            fs::copy(path, &bak)?;
            return Ok(Some(bak));
        }
    }
    Err(EngineError::Txn(format!(
        "mcp-register: cannot create backup beside {}",
        path.display()
    )))
}

/// Newest existing `<config>.bak-<ts>[-<n>]` sibling — suffix must be
/// the strict digit shape `backup_file` produces so foreign files that
/// share the `.bak-` prefix are never restored.
fn newest_backup(path: &Path) -> Option<PathBuf> {
    let prefix = format!("{}.bak-", path.display());
    let dir = path.parent()?;
    let mut best: Option<PathBuf> = None;
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let disp = p.display().to_string();
        let Some(suffix) = disp.strip_prefix(&prefix) else {
            continue;
        };
        let digits_ok = suffix
            .split('-')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        if !digits_ok {
            continue;
        }
        if best.as_ref().map_or(true, |b| disp > b.display().to_string()) {
            best = Some(p);
        }
    }
    best
}

pub struct EditOutcome {
    pub config_written: bool,
    pub backup: Option<PathBuf>,
    pub entry_removed: bool,
    pub config_deleted: bool,
}

/// Register into `path`: create `created` bytes when absent/empty, else
/// run `merge` (register_json / register_toml) over the existing text.
fn edit_config(
    path: &Path,
    created: &str,
    merge: fn(&str) -> Result<String, EngineError>,
) -> Result<EditOutcome, EngineError> {
    let cur = match fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let new: Vec<u8> = match &cur {
        None => created.as_bytes().to_vec(),
        Some(b) => {
            let s = String::from_utf8(b.clone()).map_err(|_| {
                EngineError::Txn(format!(
                    "mcp-register: {} is not UTF-8",
                    path.display()
                ))
            })?;
            if s.trim().is_empty() {
                created.as_bytes().to_vec()
            } else {
                match merge(&s) {
                    Ok(t) => t.into_bytes(),
                    Err(e) => return Err(e),
                }
            }
        }
    };
    if cur.as_deref() == Some(new.as_slice()) {
        return Ok(EditOutcome {
            config_written: false,
            backup: None,
            entry_removed: false,
            config_deleted: false,
        });
    }
    let backup = backup_file(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    util::atomic_write(path, &new)?;
    Ok(EditOutcome {
        config_written: true,
        backup,
        entry_removed: false,
        config_deleted: false,
    })
}

/// Uninstall from `path`: restore the newest backup when the file is
/// untouched since register, delete a file we created, else surgically
/// remove our entry.
fn unedit_config(
    path: &Path,
    created: &str,
    merge: fn(&str) -> Result<String, EngineError>,
    unmerge: fn(&str) -> Result<(String, bool), EngineError>,
) -> Result<EditOutcome, EngineError> {
    let cur = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(EditOutcome {
                config_written: false,
                backup: None,
                entry_removed: false,
                config_deleted: false,
            })
        }
        Err(e) => return Err(e.into()),
    };
    let s = String::from_utf8(cur.clone()).map_err(|_| {
        EngineError::Txn(format!(
            "mcp-register: {} is not UTF-8",
            path.display()
        ))
    })?;
    // Backup-restore: if nothing touched the file since registration,
    // hand back the captured pre-register bytes verbatim.
    if let Some(bak) = newest_backup(path) {
        if let Ok(bbytes) = fs::read(&bak) {
            if let Ok(btext) = String::from_utf8(bbytes.clone()) {
                let regenerated = if btext.trim().is_empty() {
                    created.to_string()
                } else {
                    merge(&btext).unwrap_or_default()
                };
                if regenerated.as_bytes() == cur.as_slice() {
                    backup_file(path)?; // spec: back up before every write
                    util::atomic_write(path, &bbytes)?;
                    return Ok(EditOutcome {
                        config_written: true,
                        backup: None,
                        entry_removed: true,
                        config_deleted: false,
                    });
                }
            }
        }
    }
    if cur == created.as_bytes() {
        // File is exactly what register would have created → remove it.
        fs::remove_file(path)?;
        return Ok(EditOutcome {
            config_written: false,
            backup: None,
            entry_removed: true,
            config_deleted: true,
        });
    }
    if s.trim().is_empty() {
        return Ok(EditOutcome {
            config_written: false,
            backup: None,
            entry_removed: false,
            config_deleted: false,
        });
    }
    let (new, removed) = unmerge(&s)?;
    if !removed {
        return Ok(EditOutcome {
            config_written: false,
            backup: None,
            entry_removed: false,
            config_deleted: false,
        });
    }
    let backup = backup_file(path)?;
    util::atomic_write(path, new.as_bytes())?;
    Ok(EditOutcome {
        config_written: true,
        backup,
        entry_removed: true,
        config_deleted: false,
    })
}

// ---------------------------------------------------------------------------
// CLAUDE.md / AGENTS.md managed block
// ---------------------------------------------------------------------------

/// Apply (install) or excise (uninstall) the managed block in `text`.
/// Returns `(new_text, changed)`. The append/removal rules mirror the
/// TOML ones: at EOF, one preceding newline is handed back iff the line
/// before the block is non-blank.
fn edit_block(text: &str, install: bool) -> (String, bool) {
    let s = text.find(BLOCK_START);
    let e = text.find(BLOCK_END);
    let has_markers = matches!((s, e), (Some(s), Some(e)) if s <= e);
    if install {
        if has_markers {
            let (s, mut e) = (s.unwrap(), e.unwrap() + BLOCK_END.len());
            if text.as_bytes().get(e) == Some(&b'\n') {
                e += 1;
            }
            let new = format!("{}{}{}", &text[..s], MEMORY_BLOCK, &text[e..]);
            let changed = new != text;
            (new, changed)
        } else if text.is_empty() {
            (MEMORY_BLOCK.to_string(), true)
        } else {
            (format!("{}\n{}", text, MEMORY_BLOCK), true)
        }
    } else {
        if !has_markers {
            return (text.to_string(), false);
        }
        let (s, e0) = (s.unwrap(), e.unwrap() + BLOCK_END.len());
        let mut e = e0;
        if text.as_bytes().get(e) == Some(&b'\n') {
            e += 1;
        }
        let mut start = s;
        if e == text.len() && s > 0 && text.as_bytes()[s - 1] == b'\n' {
            // Appends always contribute exactly one `\n` before the
            // block — hand it back so register→uninstall round-trips to
            // the original bytes.
            start = s - 1;
        }
        (format!("{}{}", &text[..start], &text[e..]), true)
    }
}

/// Per-doc status for the payload: `created`, `updated`, `unchanged`,
/// `removed`, or `absent`.
fn apply_doc_block(path: &Path, install: bool) -> Result<&'static str, EngineError> {
    let existed = path.exists();
    let cur = if existed {
        String::from_utf8(fs::read(path)?).map_err(|_| {
            EngineError::Txn(format!("mcp-register: {} is not UTF-8", path.display()))
        })?
    } else {
        String::new()
    };
    let (new, changed) = edit_block(&cur, install);
    if !changed {
        return Ok(if existed { "unchanged" } else { "absent" });
    }
    if !install && new.is_empty() {
        // The file is only our block — register created it; remove it.
        fs::remove_file(path)?;
        return Ok("removed");
    }
    util::atomic_write(path, new.as_bytes())?;
    Ok(if !install {
        "removed"
    } else if existed {
        "updated"
    } else {
        "created"
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// `mcp-register --client <name> [--uninstall]`. `root` is the resolved
/// project dir (cwd by default) — the managed CLAUDE.md/AGENTS.md block
/// lives there.
pub fn run(client: McpClient, uninstall: bool, root: &Path) -> Result<Value, EngineError> {
    let home = home_dir()?;
    let config = config_path(client, &home)?;
    let is_toml = client == McpClient::Codex;
    let created = if is_toml { CREATED_TOML } else { CREATED_JSON };
    let merge = if is_toml {
        register_toml
    } else {
        register_json
    };
    let unmerge = if is_toml {
        unregister_toml
    } else {
        unregister_json
    };

    let cli_used = try_client_cli(client, uninstall);
    let mut out = EditOutcome {
        config_written: false,
        backup: None,
        entry_removed: false,
        config_deleted: false,
    };
    if !cli_used {
        out = if uninstall {
            unedit_config(&config, created, merge, unmerge)?
        } else {
            edit_config(&config, created, merge)?
        };
    }

    let mut docs = serde_json::Map::new();
    for name in DOC_FILES {
        docs.insert(
            name.to_string(),
            json!(apply_doc_block(&root.join(name), !uninstall)?),
        );
    }

    let mut payload = serde_json::Map::new();
    payload.insert("client".into(), json!(client.as_str()));
    payload.insert(
        "action".into(),
        json!(if uninstall { "unregistered" } else { "registered" }),
    );
    payload.insert("cli_used".into(), json!(cli_used));
    payload.insert("config_path".into(), json!(config.display().to_string()));
    payload.insert("config_written".into(), json!(out.config_written));
    if let Some(b) = &out.backup {
        payload.insert("backup".into(), json!(b.display().to_string()));
    }
    if uninstall {
        payload.insert("entry_removed".into(), json!(out.entry_removed));
        payload.insert("config_deleted".into(), json!(out.config_deleted));
    }
    payload.insert("docs".into(), Value::Object(docs));
    Ok(Value::Object(payload))
}
