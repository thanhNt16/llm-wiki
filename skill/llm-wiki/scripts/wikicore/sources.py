"""Source registry: identity, versioning, raw preservation, extraction stubs.

Ingest MUST only preserve and normalize evidence — it never accepts claims
(PRD §29). Every non-trivial ingestion carries an extraction report whose
coverage warnings make extraction failure explicit, never silent (PRD §13).
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import time
from typing import Optional

from . import ids, secrets as secrets_mod
from .transaction import Transaction, TxnError

TEXT_EXTS = {
    ".md", ".markdown", ".txt", ".csv", ".tsv", ".json", ".yaml", ".yml",
    ".html", ".rst", ".xml", ".toml", ".ini", ".cfg", ".py", ".js", ".ts", ".sh",
}
BINARY_EXTS = {".pdf", ".docx", ".xlsx", ".pptx", ".png", ".jpg", ".jpeg", ".gif", ".zip"}

INJECTION_RE = re.compile(
    r"(?i)ignore\s+(all\s+)?previous\s+instructions|disregard\s+\S+\s+instructions"
    r"|you\s+are\s+now\s+a|exfiltrate|system\s+prompt\s*:"
)


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def origin_key(kind: str, ref: str) -> str:
    if kind in ("file", "directory"):
        return os.path.abspath(ref)
    if kind == "url":
        from urllib.parse import urlsplit, parse_qsl, urlunsplit

        parts = urlsplit(ref)
        query = "&".join(sorted("%s=%s" % kv for kv in parse_qsl(parts.query)))
        return urlunsplit((parts.scheme.lower(), parts.netloc.lower(), parts.path, query, ""))
    if kind == "session":
        return "session:" + ref
    if kind == "text":
        return "text:" + _sha256(ref.encode("utf-8"))[:16]
    raise TxnError("unknown source kind: %r" % kind)


def _ext(ref: str) -> str:
    return os.path.splitext(ref)[1].lower()


def _is_text_like(kind: str, ref: str) -> bool:
    if kind in ("text", "url", "session"):
        return True
    if kind == "file":
        return _ext(ref) in TEXT_EXTS
    return False  # directory/repo handled as binary-ish bundle


def resolve(wiki, kind: str, ref: str) -> Optional[str]:
    aliases = wiki.load_json(".state/aliases.json")
    return aliases.get(origin_key(kind, ref))


def _secret_gate(wiki, text: str, filename: Optional[str], warnings: list):
    """Returns (normalized_text, secret_summary). Raises TxnError on deny."""
    cfg = wiki.load_config()
    policy = cfg.get("secret_policy", "warn")
    findings = secrets_mod.scan(text, filename=filename)
    if not findings:
        return text, {"findings": [], "redacted": 0}
    summary = {"findings": findings, "redacted": 0}
    if policy == "deny":
        kinds = ", ".join(f["kind"] for f in findings)
        raise TxnError("ingest denied: likely secrets detected (%s)" % kinds)
    if policy == "redact":
        text, n = secrets_mod.redact(text)
        summary["redacted"] = n
        warnings.append("secrets redacted in normalized content (count=%d)" % n)
    else:
        warnings.append(
            "possible secrets present (kinds=%s); policy=warn so content stored as-is"
            % ",".join(f["kind"] for f in findings)
        )
    return text, summary


def _parser_for_binary(path: str):
    """Optional richer parser (markitdown). Returns (text, parser_name) or (None, None)."""
    if shutil.which("markitdown"):
        try:
            out = subprocess.run(
                ["markitdown", path], capture_output=True, timeout=120, check=True
            )
            return out.stdout.decode("utf-8", "replace"), "markitdown"
        except Exception:
            return None, None
    return None, None


def ingest(wiki, kind: str, ref: str, data: bytes, source_id: Optional[str] = None) -> dict:
    if not wiki.exists():
        raise TxnError("wiki not initialized; run wiki-init first")
    okey = origin_key(kind, ref)
    aliases = wiki.load_json(".state/aliases.json")
    sid = source_id or aliases.get(okey)
    sha = _sha256(data)
    warnings = []

    text_like = _is_text_like(kind, ref)
    filename = os.path.basename(ref) if kind in ("file", "directory") else None

    normalized_text = ""
    parser = {"name": "wikicore-raw", "version": "1.0"}
    if text_like:
        text = data.decode("utf-8", "replace")
        text, secret_summary = _secret_gate(wiki, text, filename, warnings)
        normalized_text = text
    else:
        secret_summary = {"findings": [], "redacted": 0}

    txn = Transaction(wiki, "wiki-ingest")

    if INJECTION_RE.search(normalized_text):
        warnings.append(
            "possible_prompt_injection: content contains instruction-like text; "
            "stored as evidence only — it has no authority over agent behavior"
        )

    if sid:
        manifest = wiki.load_json("sources/%s/manifest.json" % sid)
        for v in manifest["versions"]:
            if v["sha256"] == sha:
                return {
                    "source_id": sid,
                    "version": v["version"],
                    "sha256": sha,
                    "deduplicated": True,
                    "warnings": [],
                    "secrets": {"findings": [], "redacted": 0},
                    "raw_path": v["raw_path"],
                }
        version = len(manifest["versions"]) + 1
    else:
        sid = ids.new("source")
        manifest = {
            "id": sid,
            "kind": kind,
            "origin": ref if kind != "text" else "inline-text",
            "root_origin": sid,
            "created_at": _now(),
            "versions": [],
            "title": filename or ref[:80],
        }
        version = 1

    raw_rel = "raw/%s/%s/%s" % (kind, sha[:16], filename or "content.bin")
    txn.stage_write(raw_rel, data)

    norm_rel = "sources/%s/content.md" % sid
    assets = []
    coverage = {
        "text": "complete", "tables": "skipped", "images": "skipped",
        "diagrams": "skipped", "formulas": "skipped",
    }
    if text_like:
        txn.stage_write(norm_rel, normalized_text)
    else:
        parsed, parser_name = _parser_for_binary(_staging_raw_path(txn, raw_rel))
        if parsed is not None:
            parser = {"name": parser_name, "version": "?"}
            coverage["text"] = "partial"
            txn.stage_write(norm_rel, parsed)
            warnings.append("binary parsed with %s; verify coverage" % parser_name)
        else:
            coverage["text"] = "not_extracted"
            warnings.append(
                "%s preserved as raw bytes but not semantically extracted "
                "(no parser available); use a document parser to extract" % (filename or kind)
            )
            txn.stage_write(norm_rel, "")

    manifest["versions"].append({
        "version": version,
        "sha256": sha,
        "captured_at": _now(),
        "raw_path": raw_rel,
        "normalized_path": norm_rel,
        "assets": assets,
        "adapter": "wikicore.%s" % kind,
        "bytes": len(data),
    })
    txn.stage_write("sources/%s/manifest.json" % sid, manifest)

    report = {
        "source_id": sid,
        "source_version": version,
        "coverage": coverage,
        "warnings": warnings,
        "parser": parser,
    }
    extraction = {"extraction_report": report, "candidates": [], "source_version": version}
    txn.stage_write("sources/%s/extraction.json" % sid, extraction)

    def _alias_update(a):
        a[okey] = sid

    txn.stage_state(".state/aliases.json", _alias_update)

    txn.changes = {"sources_registered": 1 if version == 1 else 0, "versions_added": 1}
    txn.warnings = warnings
    txn.commit(wiki.revision())

    return {
        "source_id": sid,
        "version": version,
        "sha256": sha,
        "deduplicated": False,
        "raw_path": raw_rel,
        "normalized_path": norm_rel,
        "coverage": coverage,
        "warnings": warnings,
        "secrets": secret_summary,
        "run_id": txn.run_id,
    }


def _staging_raw_path(txn: Transaction, raw_rel: str) -> str:
    return os.path.join(txn.staging_dir, raw_rel)


def load_content(wiki, source_id: str, version: int) -> str:
    manifest = wiki.load_json("sources/%s/manifest.json" % source_id)
    for v in manifest["versions"]:
        if v["version"] == version:
            path = wiki.p(v["normalized_path"])
            if os.path.isfile(path):
                with open(path) as f:
                    return f.read()
            return ""
    raise TxnError("unknown version %d of %s" % (version, source_id))


def get_manifest(wiki, source_id: str) -> dict:
    return wiki.load_json("sources/%s/manifest.json" % source_id)


def pending_versions(wiki) -> list:
    state = wiki.state()
    compiled = state.get("compiled", {})
    pending = []
    if not os.path.isdir(wiki.p("sources")):
        return pending
    for sid in sorted(os.listdir(wiki.p("sources"))):
        mpath = wiki.p("sources", sid, "manifest.json")
        if not os.path.isfile(mpath):
            continue
        manifest = wiki.load_json("sources/%s/manifest.json" % sid)
        done = compiled.get(sid, [])
        for v in manifest["versions"]:
            if v["version"] not in done:
                pending.append({
                    "source_id": sid,
                    "version": v["version"],
                    "origin": manifest["origin"],
                    "title": manifest.get("title", ""),
                })
    return pending


def mark_compiled(wiki, source_id: str, version: int) -> None:
    txn = Transaction(wiki, "wiki-internal-mark-compiled")

    def _upd(state):
        state.setdefault("compiled", {}).setdefault(source_id, []).append(version)

    txn.stage_state(".state/manifest.json", _upd)
    txn.commit(wiki.revision())
