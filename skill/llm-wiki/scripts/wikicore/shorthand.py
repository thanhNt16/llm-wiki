"""Shorthand candidate parsing for the compile umbrella (spec §2).

Agents write one JSON object per line with only semantic fields; this module
expands locator/authority shorthand and stamps engine-owned fields.
"""
import json
import re
import time

from .transaction import TxnError

_AUTHORITY_MAP = {
    "manual": "authoritative_source", "doc": "authoritative_source",
    "config": "authoritative_source",
    "decision": "explicit_project_decision", "adr": "explicit_project_decision",
    "code": "implementation_verification", "verified": "implementation_verification",
    "inferred": "agent_inference",
}
_AUTHORITY_SOURCE = {
    "manual": "manual", "doc": "doc", "config": "config",
    "decision": "decision", "adr": "adr",
    "code": "code", "verified": "verified", "inferred": "agent",
}

_AGENT_FIELDS = {"subject", "predicate", "value", "locator", "authority",
                 "scope", "valid_from", "supersedes", "evidence"}


def expand_locator(raw):
    if isinstance(raw, dict):
        return raw
    if not isinstance(raw, str):
        raise TxnError("locator must be a string shorthand or object, got %r" % type(raw))
    m = re.match(r"^([hls]):(.+)$", raw, re.S)
    if not m:
        raise TxnError("bad locator shorthand %r — use h:<heading>, l:<lines>, s:<section>" % raw)
    kind, val = m.group(1), m.group(2).strip()
    return {"type": {"h": "heading", "l": "line_range", "s": "section"}[kind],
            "value": val}


def expand_authority(raw):
    if isinstance(raw, dict):
        return raw
    if not isinstance(raw, str):
        raise TxnError("authority must be a shorthand string or object, got %r" % type(raw))
    key, _, src = raw.partition(":")
    key = key.strip().lower()
    if key not in _AUTHORITY_MAP:
        raise TxnError("bad authority shorthand %r — one of %s" % (raw, sorted(_AUTHORITY_MAP)))
    return {"type": _AUTHORITY_MAP[key], "source": src.strip() or _AUTHORITY_SOURCE[key]}


def normalize_candidate(c, source_id, source_version, root_origin):
    """Shorthand dict -> full candidate. Engine stamps id later (staging),
    plus recorded_at/status/proposed_by/evidence/root_origin here."""
    if not isinstance(c, dict):
        raise TxnError("candidate must be an object")
    for field in ("subject", "predicate"):
        if not c.get(field):
            raise TxnError("candidate missing %r" % field)
    out = dict(c)
    out["subject"] = str(out["subject"]).strip()
    out["predicate"] = str(out["predicate"]).strip()
    out["locator_used"] = None
    del out["locator_used"]
    if "evidence" not in out:
        loc = expand_locator(out.pop("locator", "s:whole-document"))
        out["evidence"] = [{"source_id": source_id, "source_version": source_version,
                            "locator": loc}]
    else:
        out.pop("locator", None)
    out.setdefault("authority", {"type": "authoritative_source", "source": "document"})
    out["authority"] = expand_authority(out["authority"])
    out.setdefault("scope", {})
    out.setdefault("valid_from", None)
    out.setdefault("valid_to", None)
    out.setdefault("supersedes", [])
    out["recorded_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    out["status"] = "candidate"
    out["proposed_by"] = "agent"
    out["root_origin"] = root_origin
    return out


def parse_candidates_file(path):
    """.jsonl -> one shorthand object per line; .json -> array or {candidates:[..]}.
    Returns raw dicts (normalization happens at staging where source_id is known)."""
    if path.endswith(".jsonl"):
        out = []
        with open(path) as f:
            for n, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError as e:
                    raise TxnError("candidates file %s line %d: %s" % (path, n, e))
                if not isinstance(obj, dict):
                    raise TxnError("candidates file %s line %d: not an object" % (path, n))
                out.append(obj)
        return out
    with open(path) as f:
        data = json.load(f)
    if isinstance(data, dict):
        data = data.get("candidates", [])
    if not isinstance(data, list):
        raise TxnError("candidates file %s: expected array" % path)
    return data
