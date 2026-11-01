"""SP2b: mechanical evidence re-resolution for claims.

Read-only. For every claim evidence ref: re-read the pinned source version,
re-hash it against the ingest-time sha256, and re-locate the locator
(text / line_range / heading). Each claim is classified

  verified    — content file intact (sha matches) and locator resolves
  stale       — file intact but the locator no longer resolves (content
                drifted from what the claim cites), or the stored bytes'
                hash no longer matches the ingest-time hash
  unresolved  — source manifest or pinned version missing/corrupt

Never mutates state; the caller decides what to do with the report.
"""
import hashlib
import os
import re

from . import claims as claims_mod


def _sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def _resolve_locator(loc: dict, text: str, lines: list) -> bool:
    """Does the locator still point at the cited content?"""
    if not isinstance(loc, dict):
        return False
    t = loc.get("type")
    v = loc.get("value")
    if t == "text":
        return isinstance(v, str) and v in text
    if t == "line_range":
        if not isinstance(v, str):
            return False
        m = re.match(r"^(\d+)(?:-(\d+))?$", v.strip())
        if not m:
            return False
        a = int(m.group(1))
        b = int(m.group(2)) if m.group(2) else a
        return 1 <= a <= b <= len(lines)
    if t == "heading":
        if not isinstance(v, str):
            return False
        want = v.strip()
        for ln in lines:
            s = ln.strip()
            if s == want or re.sub(r"^#{1,6}\s+", "", s) == want:
                return True
        return False
    return False


def check_evidence(wiki, details_limit: int = 100) -> dict:
    counts = {"verified": 0, "stale": 0, "unresolved": 0}
    details = []
    for claim in claims_mod.list_claims(wiki):
        status = "verified"
        reasons = []
        for ev in claim.get("evidence", []):
            sid = ev.get("source_id")
            ver = ev.get("source_version")
            if not sid:
                reasons.append("evidence ref missing source_id"); status = "unresolved"
                continue
            try:
                manifest = wiki.load_json("sources/%s/manifest.json" % sid)
            except (OSError, ValueError):
                reasons.append("source %s manifest missing/corrupt" % sid); status = "unresolved"
                continue
            ver_entry = next((x for x in manifest.get("versions", [])
                              if x.get("version") == ver), None)
            if ver_entry is None:
                reasons.append("source %s has no version %r" % (sid, ver)); status = "unresolved"
                continue
            rel = ver_entry.get("normalized_path") or \
                "sources/%s/content.md" % sid
            path = wiki.p(*rel.split("/"))
            if not os.path.isfile(path):
                reasons.append("content file missing: %s" % rel); status = "unresolved"
                continue
            if _sha256_file(path) != ver_entry.get("sha256"):
                # stored bytes no longer match the ingest-time hash
                reasons.append("%s hash differs from ingest-time sha256" % rel); status = "stale"
                continue
            with open(path, "r", encoding="utf-8", errors="replace") as f:
                text = f.read()
            if not _resolve_locator(ev.get("locator"), text, text.splitlines()):
                reasons.append("locator %r no longer resolves in %s" % (ev.get("locator"), rel)); status = "stale"
        counts[status] += 1
        if status != "verified" and len(details) < details_limit:
            details.append({"claim_id": claim["id"], "status": status,
                            "reasons": reasons[:5]})
    return {"checked_claims": sum(counts.values()), **counts,
            "ok": counts["stale"] == 0 and counts["unresolved"] == 0,
            "details": details}
