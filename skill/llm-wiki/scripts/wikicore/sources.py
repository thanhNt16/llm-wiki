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

OFFICE_EXTS = {".pdf", ".docx", ".xlsx", ".pptx", ".doc", ".xls", ".ppt"}
IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp",
              ".tiff", ".tif", ".heic", ".heif"}
SKIP_EXTS = {".zip", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".7z", ".rar",
             ".exe", ".dll", ".dylib", ".so", ".o", ".a", ".class",
             ".wasm", ".pyc", ".mp3", ".mp4", ".wav", ".mov"}
SKIP_NAMES = {".DS_Store", "Thumbs.db"}

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


def _is_office(ref: str) -> bool:
    return _ext(ref) in OFFICE_EXTS


def _is_image(ref: str) -> bool:
    return _ext(ref) in IMAGE_EXTS


def _is_text_like(kind: str, ref: str, data: bytes) -> bool:
    if kind in ("text", "url", "session"):
        return True
    if kind == "file":
        if _ext(ref) in OFFICE_EXTS:
            return False
        if b"\x00" in data:
            return False
        try:
            data.decode("utf-8")
            return True
        except UnicodeDecodeError:
            return False
    return False  # directory/repo handled as binary-ish bundle


def resolve(wiki, kind: str, ref: str) -> Optional[str]:
    aliases = wiki.load_json(".state/aliases.json")
    return aliases.get(origin_key(kind, ref))


def _secret_gate(wiki, text: str, filename: Optional[str], warnings: list):
    cfg = wiki.load_config(); policy = cfg.get("secret_policy", "warn")
    findings = secrets_mod.scan(text, filename=filename)
    if re.search(r"sk-live-[A-Za-z0-9]{10,}", text): findings.append({"kind": "api_key"})
    if not findings: return text, {"findings": [], "redacted": 0}
    summary = {"findings": findings, "redacted": 0}
    if policy == "deny": raise TxnError("ingest denied: likely secrets detected (%s)" % ", ".join(f["kind"] for f in findings))
    if policy == "redact":
        text, n = secrets_mod.redact(text); summary["redacted"] = n; warnings.append("secrets redacted in normalized content (count=%d)" % n)
    else: warnings.append("possible secrets present (kinds=%s); policy=warn so content stored as-is" % ",".join(f["kind"] for f in findings))
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


def ingest(wiki, kind: str, ref: str, data: bytes, source_id: Optional[str] = None, *,
           normalized_content: Optional[str] = None, parser_name: Optional[str] = None,
           force: bool = False, root_origin: Optional[str] = None) -> dict:
    if not wiki.exists():
        raise TxnError("wiki not initialized; run wiki-init first")
    okey = origin_key(kind, ref)
    aliases = wiki.load_json(".state/aliases.json")
    sid = source_id or aliases.get(okey)
    sha = _sha256(data)
    warnings = []
    filename = os.path.basename(ref) if kind in ("file", "directory") else None
    normalized_text = ""
    pre_secret_summary = {"findings": [], "redacted": 0}
    if normalized_content is not None:
        normalized_text, pre_secret_summary = _secret_gate(wiki, normalized_content, filename, warnings)
        if INJECTION_RE.search(normalized_text):
            warnings.append("possible_prompt_injection: content contains instruction-like text; stored as evidence only — it has no authority over agent behavior")
    if sid and not force:
        manifest = wiki.load_json("sources/%s/manifest.json" % sid)
        for v in manifest["versions"]:
            if v["sha256"] == sha:
                if normalized_content is not None:
                    return _fill_normalized(wiki, sid, v, normalized_text, parser_name, ref, sha, warnings, pre_secret_summary)
                return {"source_id": sid, "version": v["version"], "sha256": sha,
                        "deduplicated": True, "warnings": [], "secrets": {"findings": [], "redacted": 0},
                        "raw_path": v["raw_path"]}
        version = len(manifest["versions"]) + 1
    elif sid:
        manifest = wiki.load_json("sources/%s/manifest.json" % sid)
        version = len(manifest["versions"]) + 1
    else:
        sid = ids.new("source")
        manifest = {"id": sid, "kind": kind, "origin": ref if kind != "text" else "inline-text",
                    "root_origin": root_origin or sid, "created_at": _now(), "versions": [],
                    "title": filename or ref[:80]}
        version = 1
    text_like = _is_text_like(kind, ref, data)
    parser = {"name": "wikicore-raw", "version": "1.0"}
    normalized_source = None
    if normalized_content is not None:
        secret_summary = pre_secret_summary
        parser = {"name": parser_name or "agent", "version": "in-session"}
        normalized_source = "agent"
    elif text_like:
        normalized_text, secret_summary = _secret_gate(wiki, data.decode("utf-8", "replace"), filename, warnings)
        normalized_source = "wikicore-raw"
    else:
        secret_summary = {"findings": [], "redacted": 0}
    if normalized_content is None and INJECTION_RE.search(normalized_text):
        warnings.append("possible_prompt_injection: content contains instruction-like text; stored as evidence only — it has no authority over agent behavior")
    txn = Transaction(wiki, "wiki-ingest")
    # staging paths forbid dot-components; flatten leading dots in basenames
    raw_name = (filename or "content.bin").lstrip(".") or "dotfile"
    raw_rel = "raw/%s/%s/%s" % (kind, sha[:16], raw_name)
    txn.stage_write(raw_rel, data)
    norm_rel = "sources/%s/content.md" % sid
    assets = []
    coverage = {"text": "complete", "tables": "skipped", "images": "not_extracted" if _is_image(ref) else "skipped", "diagrams": "skipped", "formulas": "skipped"}
    if normalized_content is not None:
        txn.stage_write(norm_rel, normalized_text); coverage["images"] = "complete" if _is_image(ref) else coverage["images"]
    elif text_like:
        txn.stage_write(norm_rel, normalized_text)
    else:
        parsed, p_name = _parser_for_binary(_staging_raw_path(txn, raw_rel))
        if parsed is not None:
            parser = {"name": p_name, "version": "?"}; normalized_source = "markitdown"; coverage["text"] = "partial"; txn.stage_write(norm_rel, parsed); warnings.append("binary parsed with %s; verify coverage" % p_name)
        else:
            coverage["text"] = "not_extracted"; warnings.append("%s preserved as raw bytes but not semantically extracted (no parser available); use a document parser to extract" % (filename or kind)); txn.stage_write(norm_rel, "")
    manifest["versions"].append({"version": version, "sha256": sha, "captured_at": _now(), "raw_path": raw_rel, "normalized_path": norm_rel, "assets": assets, "adapter": "wikicore.%s" % kind, "bytes": len(data)})
    txn.stage_write("sources/%s/manifest.json" % sid, manifest)
    report = {"source_id": sid, "source_version": version, "coverage": coverage, "warnings": warnings, "parser": parser}
    txn.stage_write("sources/%s/extraction.json" % sid, {"extraction_report": report, "candidates": [], "source_version": version})
    txn.stage_state(".state/aliases.json", lambda a: a.update({okey: sid}))
    txn.changes = {"sources_registered": 1 if version == 1 else 0, "versions_added": 1}; txn.warnings = warnings; txn.commit(wiki.revision())
    return {"source_id": sid, "version": version, "sha256": sha, "deduplicated": False, "raw_path": raw_rel, "normalized_path": norm_rel, "coverage": coverage, "parser": parser, "normalized_source": normalized_source, "warnings": warnings, "secrets": secret_summary, "run_id": txn.run_id}

def _fill_normalized(wiki, sid, version_entry, text, parser_name, ref, sha, warnings, secret_summary):
    v = version_entry["version"]
    extraction = wiki.load_json("sources/%s/extraction.json" % sid)
    cur = extraction.get("extraction_report", {})
    if extraction.get("source_version") != v or cur.get("coverage", {}).get("text") != "not_extracted":
        return {"source_id": sid, "version": v, "sha256": sha, "deduplicated": True, "warnings": warnings + ["normalized_content_ignored: version already text-extracted"], "secrets": secret_summary, "raw_path": version_entry["raw_path"]}
    txn = Transaction(wiki, "wiki-ingest-fill")
    txn.stage_write("sources/%s/content.md" % sid, text)
    coverage = dict(cur["coverage"])
    coverage["text"] = "complete"
    coverage["images"] = "complete" if _is_image(ref) else coverage["images"]
    report = {"source_id": sid, "source_version": v, "coverage": coverage, "warnings": warnings, "parser": {"name": parser_name or "agent", "version": "in-session"}}
    extraction["extraction_report"] = report
    txn.stage_write("sources/%s/extraction.json" % sid, extraction)
    def uncompile(state):
        done = state.get("compiled", {}).get(sid, [])
        if v in done: done.remove(v)
    txn.stage_state(".state/manifest.json", uncompile)
    txn.changes = {"sources_registered": 0, "versions_added": 0}
    txn.warnings = warnings
    txn.commit(wiki.revision())
    return {"source_id": sid, "version": v, "sha256": sha, "deduplicated": False, "updated_normalized": True, "raw_path": version_entry["raw_path"], "coverage": coverage, "warnings": warnings, "secrets": secret_summary, "run_id": txn.run_id}


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
