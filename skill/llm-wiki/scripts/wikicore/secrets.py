"""Secret heuristics gating durable ingestion (PRD §50).

Findings are counts only — secret VALUES must never reach logs or receipts.
"""
import re

_PATTERNS = [
    ("private_key", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("aws_key", re.compile(r"AKIA[0-9A-Z]{16}")),
    ("github_token", re.compile(r"gh[pousr]_[A-Za-z0-9]{30,}")),
    ("slack_token", re.compile(r"xox[bpars]-[A-Za-z0-9-]{10,}")),
    ("google_api_key", re.compile(r"AIza[0-9A-Za-z\-_]{35}")),
    ("url_credentials", re.compile(r"[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@")),
    ("credential_assignment", re.compile(
        r"(?i)\b(password|passwd|pwd|secret|api_?key|access_?key|auth_?token|token)\b"
        r"\s*[=:]\s*['\"]?([^\s'\"]{8,})")),
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
    return findings


def os_path_basename(path: str) -> str:
    return path.replace("\\", "/").rsplit("/", 1)[-1]


def redact(data: str):
    """Replace secret values with REDACTED. Returns (text, replacements)."""
    n = 0

    def _bump(_m):
        nonlocal n
        n += 1
        return "REDACTED"

    out = data
    for kind, pattern in _PATTERNS:
        if kind == "credential_assignment":
            out = pattern.sub(lambda m: m.group(1) + "=REDACTED", out)
            n += len(pattern.findall(data))
        else:
            out = pattern.sub(_bump, out)
    return out, n
