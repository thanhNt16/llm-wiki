"""Secret heuristics gating durable ingestion (PRD §50).

Findings are counts only — secret VALUES must never reach logs or receipts.
All detection patterns live in _PATTERNS so scan() and redact() can never
diverge: every finding kind scan() reports is also redacted here.
"""
import re

_PATTERNS = [
    ("private_key", re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")),
    ("aws_key", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("github_token", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{30,}\b")),
    ("github_token", re.compile(r"\bgithub_pat_[A-Za-z0-9_]{22,}\b")),
    ("slack_token", re.compile(r"\bxox[bpars]-[A-Za-z0-9-]{10,}\b")),
    ("slack_token", re.compile(r"\bxapp-[A-Za-z0-9-]{10,}\b")),
    ("google_api_key", re.compile(r"\bAIza[0-9A-Za-z\-_]{35}\b")),
    # OpenAI-style keys: sk-live-*, sk-proj-*, and generic sk-* secrets.
    ("api_key", re.compile(r"\bsk-(?:live|proj)-[A-Za-z0-9_-]{10,}\b")),
    ("api_key", re.compile(r"\bsk-[A-Za-z0-9]{20,}\b")),
    # JSON Web Tokens (three base64url segments, always start with eyJ).
    ("jwt", re.compile(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}\b")),
    ("url_credentials", re.compile(r"[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@")),
    # Runs last so redacted concrete tokens are not double-counted.
    ("credential_assignment", re.compile(
        r"(?i)\b(password|passwd|pwd|secret|api_?key|access_?key|auth_?token|token)\b"
        r"(\s*[=:]\s*|\s+)['\"]?([^\s'\"]{8,})")),
]

_ENV_RE = re.compile(r"^\.env(\..+)?$", re.I)


def scan(data: str, filename=None) -> list:
    findings = []
    for kind, pattern in _PATTERNS:
        n = len(pattern.findall(data))
        if n:
            findings.append({"kind": kind, "count": n})
    if filename and _ENV_RE.match(os_path_basename(filename)):
        findings.append({"kind": "env_file", "count": 1})
    return _merge_counts(findings)


def _merge_counts(findings: list) -> list:
    """Multiple regexes may share a kind; report one count per kind."""
    merged = {}
    order = []
    for f in findings:
        if f["kind"] not in merged:
            merged[f["kind"]] = dict(f)
            order.append(f["kind"])
        else:
            merged[f["kind"]]["count"] += f["count"]
    return [merged[k] for k in order]


def os_path_basename(path: str) -> str:
    return path.replace("\\", "/").rsplit("/", 1)[-1]


def redact(data: str):
    """Replace secret values with REDACTED. Returns (text, replacements).

    The credential-assignment pattern preserves the original separator
    (`=`, `:`, or whitespace); every replacement is counted as it happens
    so `n` matches the text actually produced.
    """
    n = 0

    def _bump(_m):
        nonlocal n
        n += 1
        return "REDACTED"

    def _credential(m):
        nonlocal n
        if m.group(3) == "REDACTED":  # already redacted by an earlier pattern
            return m.group(0)
        n += 1
        return m.group(1) + m.group(2) + "REDACTED"

    out = data
    for kind, pattern in _PATTERNS:
        if kind == "credential_assignment":
            out = pattern.sub(_credential, out)
        else:
            out = pattern.sub(_bump, out)
    return out, n
