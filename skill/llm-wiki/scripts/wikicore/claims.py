"""Claim and decision stores.

Claims are structured interpretations of evidence; decisions are what the
project deliberately chose (PRD §3). A claim is NEVER promoted to a decision
by repetition (PRD §3, Invariant 2).
"""
import datetime
import json
import os
from typing import Optional

from . import ids
from .schema import validate as schema_validate
from .transaction import Transaction, TxnError

_SCHEMAS = os.path.join(os.path.dirname(__file__), "..", "..", "schemas")


def _schema(name: str) -> dict:
    with open(os.path.join(_SCHEMAS, name)) as f:
        return json.load(f)


CLAIM_SCHEMA = _schema("claim.schema.json")
DECISION_SCHEMA = _schema("decision.schema.json")

STATUSES = ("candidate", "provisional", "accepted", "disputed", "superseded", "rejected")
TERMINAL = ("superseded", "rejected")


def _claim_path(cid: str) -> str:
    return "claims/%s.json" % cid


def load_claim(wiki, cid: str) -> dict:
    if not isinstance(cid, str) or not cid.startswith("claim_") or not ids.validate(cid):
        raise TxnError("claim id invalid: %r" % cid)
    path = wiki.p(_claim_path(cid))
    if not os.path.isfile(path):
        raise TxnError("unknown claim: %s" % cid)
    with open(path) as f:
        return json.load(f)


def save_claim(txn: Transaction, claim: dict) -> dict:
    cid = claim.get("id", "")
    if not ids.validate(cid):
        raise TxnError("claim id invalid: %r" % cid)
    existing = None
    path = txn.wiki.p(_claim_path(cid))
    if os.path.isfile(path):
        with open(path) as f:
            existing = json.load(f)
    if existing:
        claim["version"] = existing["version"] + 1
        claim["recorded_at"] = existing["recorded_at"]  # immutable
        claim["updated_at"] = _now()
        if existing.get("status") in TERMINAL and claim.get("status") not in TERMINAL:
            raise TxnError("claim %s is %s; cannot return to %s"
                           % (cid, existing.get("status"), claim.get("status")))
    else:
        # creation: callers may not inject a version; version bumps on update only
        claim["version"] = 1
        claim.setdefault("recorded_at", _now())
    if claim.get("status") in TERMINAL and not (
            claim.get("supersedes") or claim.get("superseded_by")
            or (claim.get("acceptance") or {}).get("reason")):
        raise TxnError("terminal status on %s requires supersedes/superseded_by/reason" % cid)
    errors = schema_validate(claim, CLAIM_SCHEMA)
    if errors:
        raise TxnError("claim %s invalid: %s" % (cid, "; ".join(errors)))
    txn.stage_write(_claim_path(cid), claim)
    return claim


def list_claims(wiki, status: Optional[str] = None, subject: Optional[str] = None) -> list:
    out = []
    cdir = wiki.p("claims")
    if not os.path.isdir(cdir):
        return out
    for name in sorted(os.listdir(cdir)):
        if not (name.startswith("claim_") and name.endswith(".json")):
            continue
        with open(os.path.join(cdir, name)) as f:
            c = json.load(f)
        if status and c.get("status") != status:
            continue
        if subject and c.get("subject") != subject:
            continue
        out.append(c)
    return out


def supersede(txn: Transaction, old: dict, new_id: str, policy_change: bool,
              new_valid_from: Optional[str]) -> dict:
    """CORRECTION: old stops immediately. POLICY_CHANGE: old valid until day before."""
    old["status"] = "superseded"
    old["superseded_by"] = new_id
    if policy_change and new_valid_from:
        d = datetime.date.fromisoformat(new_valid_from) - datetime.timedelta(days=1)
        old["valid_to"] = d.isoformat()
    return old


def _decision_path(did: str) -> str:
    return "decisions/%s.json" % did


def load_decision(wiki, did: str) -> dict:
    if not isinstance(did, str) or not did.startswith("decision_") or not ids.validate(did):
        raise TxnError("decision id invalid: %r" % did)
    path = wiki.p(_decision_path(did))
    if not os.path.isfile(path):
        raise TxnError("unknown decision: %s" % did)
    with open(path) as f:
        return json.load(f)


def save_decision(txn: Transaction, decision: dict) -> dict:
    did = decision.get("id", "")
    if not ids.validate(did):
        raise TxnError("decision id invalid: %r" % did)
    path = txn.wiki.p(_decision_path(did))
    if os.path.isfile(path):
        with open(path) as f:
            existing = json.load(f)
        decision["version"] = existing["version"] + 1
        decision["recorded_at"] = existing["recorded_at"]
    else:
        # creation: callers may not inject a version (same rule as save_claim)
        decision["version"] = 1
        decision.setdefault("recorded_at", _now())
    errors = schema_validate(decision, DECISION_SCHEMA)
    if errors:
        raise TxnError("decision %s invalid: %s" % (did, "; ".join(errors)))
    txn.stage_write(_decision_path(did), decision)
    return decision


def list_decisions(wiki) -> list:
    out = []
    ddir = wiki.p("decisions")
    if not os.path.isdir(ddir):
        return out
    for name in sorted(os.listdir(ddir)):
        if not (name.startswith("decision_") and name.endswith(".json")):
            continue
        with open(os.path.join(ddir, name)) as f:
            out.append(json.load(f))
    return out


def auto_decision_stub(wiki, txn: Transaction, claim: dict):
    """Mint an accepted decision stub for an accepted claim whose authority is
    explicit_project_decision (spec §3). Idempotent: a claim already linked
    from any decision's claims[] mints nothing. Returns the decision dict or
    None."""
    authority = claim.get("authority") or {}
    if authority.get("type") != "explicit_project_decision":
        return None
    if any(claim["id"] in d.get("claims", []) for d in list_decisions(wiki)):
        return None
    decision = {
        "id": ids.new("decision"),
        "title": claim["subject"],
        "body": "%s = %s" % (claim.get("predicate"), claim.get("value")),
        "status": "accepted",
        "decided_on": (claim.get("recorded_at") or _now())[:10],
        "claims": [claim["id"]],
        "evidence": claim.get("evidence", []),
        "authority": {"type": "explicit_project_decision"},
        "auto_materialized": True,
    }
    return save_decision(txn, decision)


def independence(wiki, claim: dict) -> int:
    """Distinct independent evidence origins — NOT reference count (PRD §7).

    Fails closed: an evidence ref whose manifest is missing/corrupt cannot be
    attributed to an origin, so it is EXCLUDED rather than counted as its own
    origin (which would inflate corroboration on corrupt state).
    """
    origins = set()
    for ev in claim.get("evidence", []):
        ro = ev.get("root_origin")
        if ro:
            origins.add(ro)
            continue
        sid = ev.get("source_id")
        if not sid:
            raise TxnError("evidence ref missing source_id: %r" % ev)
        try:
            manifest = wiki.load_json("sources/%s/manifest.json" % sid)
        except (OSError, ValueError):
            continue  # unreadable manifest: unverifiable, not an origin
        origins.add(manifest.get("root_origin") or sid)
    return len(origins)


def _now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def create_decision(wiki, data: dict) -> dict:
    """Validate + commit a decision record (record-decision command).
    Caller supplies title/status/decided_on/claims/evidence/body/scope;
    engine stamps id, recorded_at, version."""
    if not isinstance(data, dict):
        raise TxnError("decision input must be a JSON object")
    for banned in ("id", "recorded_at", "version"):
        if banned in data:
            raise TxnError("decision input must not include %r — engine-stamped" % banned)
    if not isinstance(data.get("claims", []), list):
        raise TxnError("decision input 'claims' must be a list")
    if not isinstance(data.get("evidence", []), list):
        raise TxnError("decision input 'evidence' must be a list")
    for cid in data.get("claims") or []:
        c = load_claim(wiki, cid)
        if c.get("status") in TERMINAL:
            raise TxnError("claim %s is %s; cannot support a decision" % (cid, c["status"]))
    from . import sources as sources_mod
    for i, ev in enumerate(data.get("evidence") or []):
        if not isinstance(ev, dict):
            raise TxnError("evidence[%d] must be a JSON object" % i)
        sid = ev.get("source_id", "")
        try:
            manifest = sources_mod.get_manifest(wiki, sid)
        except TxnError:
            manifest = None  # malformed sid -> same "unknown source" surface
        if manifest is None:
            raise TxnError("unknown source: %s" % sid)
    decision = dict(data)
    decision["id"] = ids.new("decision")
    decision["recorded_at"] = _now()
    decision["version"] = 1
    txn = Transaction(wiki, "wiki-record-decision")
    save_decision(txn, decision)
    txn.changes = {"decisions_created": 1}
    receipt = txn.commit(wiki.revision())
    return {"id": decision["id"], "decision": decision, "receipt": receipt}
