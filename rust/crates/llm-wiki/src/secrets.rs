//! Secret heuristics gating durable ingestion (PRD §50) — port of
//! `skill/llm-wiki/scripts/wikicore/secrets.py`.
//!
//! Findings are counts only — secret VALUES must never reach logs or
//! receipts. All detection patterns live in `PATTERNS` so `scan` and
//! `redact` can never diverge: every finding kind `scan` reports is also
//! redacted here.

use std::sync::LazyLock;

use regex::Regex;

/// One count-merged finding (`{"kind", "count"}`) — never carries a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: String,
    pub count: usize,
}

/// `(kind, pattern)` — ORDER IS CONTRACTUAL. `credential_assignment` runs
/// last so concrete tokens already redacted are not double-counted.
static PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("private_key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
        ("aws_key", r"\bAKIA[0-9A-Z]{16}\b"),
        ("github_token", r"\bgh[pousr]_[A-Za-z0-9]{30,}\b"),
        ("github_token", r"\bgithub_pat_[A-Za-z0-9_]{22,}\b"),
        ("slack_token", r"\bxox[bpars]-[A-Za-z0-9-]{10,}\b"),
        ("slack_token", r"\bxapp-[A-Za-z0-9-]{10,}\b"),
        ("google_api_key", r"\bAIza[0-9A-Za-z\-_]{35}\b"),
        // OpenAI-style keys: sk-live-*, sk-proj-*, and generic sk-* secrets.
        ("api_key", r"\bsk-(?:live|proj)-[A-Za-z0-9_-]{10,}\b"),
        ("api_key", r"\bsk-[A-Za-z0-9]{20,}\b"),
        // JSON Web Tokens (three base64url segments, always start with eyJ).
        ("jwt", r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}\b"),
        ("url_credentials", r"[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@"),
        // Runs last so redacted concrete tokens are not double-counted.
        ("credential_assignment",
         r#"(?i)\b(password|passwd|pwd|secret|api_?key|access_?key|auth_?token|token)\b(\s*[=:]\s*|\s+)['\"]?([^\s'\"]{8,})"#),
    ]
    .iter()
    .map(|(kind, pat)| (*kind, Regex::new(pat).expect("secrets pattern compiles")))
    .collect()
});

static ENV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\.env(\..+)?$").expect("env pattern compiles"));

/// `scan` (secrets.py:32-40): count-only findings per kind, plus the
/// `.env`-filename boost. Values never appear in the result.
pub fn scan(data: &str, filename: Option<&str>) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    for (kind, pattern) in PATTERNS.iter() {
        let n = pattern.find_iter(data).count();
        if n > 0 {
            findings.push(Finding {
                kind: kind.to_string(),
                count: n,
            });
        }
    }
    if let Some(f) = filename {
        if ENV_RE.is_match(basename(f)) {
            findings.push(Finding {
                kind: "env_file".to_string(),
                count: 1,
            });
        }
    }
    merge_counts(findings)
}

/// `_merge_counts` (secrets.py:43-53): multiple regexes may share a kind;
/// report one count per kind, first-seen order preserved.
fn merge_counts(findings: Vec<Finding>) -> Vec<Finding> {
    let mut merged: Vec<Finding> = Vec::new();
    for f in findings {
        match merged.iter_mut().find(|m| m.kind == f.kind) {
            Some(m) => m.count += f.count,
            None => merged.push(f),
        }
    }
    merged
}

/// `os.path.basename` port (secrets.py:56-57): treats `\` and `/` as
/// separators.
fn basename(path: &str) -> &str {
    path.rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(path)
}

/// `redact` (secrets.py:60-87): replace secret values with REDACTED.
/// Returns `(text, replacements)`. The credential-assignment pattern
/// preserves the original separator (`=`, `:`, or whitespace); every
/// replacement is counted as it happens so `n` matches the text actually
/// produced.
pub fn redact(data: &str) -> (String, usize) {
    let mut n = 0usize;
    let mut out = data.to_string();
    for (kind, pattern) in PATTERNS.iter() {
        if *kind == "credential_assignment" {
            out = pattern
                .replace_all(&out, |caps: &regex::Captures| {
                    if caps.get(3).map(|m| m.as_str()) == Some("REDACTED") {
                        // already redacted by an earlier pattern
                        return caps.get(0).unwrap().as_str().to_string();
                    }
                    n += 1;
                    format!("{}{}REDACTED", &caps[1], &caps[2])
                })
                .into_owned();
        } else {
            out = pattern
                .replace_all(&out, |_caps: &regex::Captures| {
                    n += 1;
                    "REDACTED".to_string()
                })
                .into_owned();
        }
    }
    (out, n)
}
