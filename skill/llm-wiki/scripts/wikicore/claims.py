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
        if existing["status"] in TERMINAL and claim["status"] not in TERMINAL:
            raise TxnError("claim %s is %s; cannot return to %s"
                           % (cid, existing["status"], claim["status"]))
    else:
        claim.setdefault("version", 1)
        claim.setdefault("recorded_at", _now())
    if claim["status"] in TERMINAL and not (claim.get("supersedes") or claim.get("superseded_by")
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
        decision.setdefault("version", 1)
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


def independence(wiki, claim: dict) -> int:
    """Distinct independent evidence origins — NOT reference count (PRD §7)."""
    origins = set()
    for ev in claim.get("evidence", []):
        ro = ev.get("root_origin")
        if not ro:
            try:
                manifest = wiki.load_json("sources/%s/manifest.json" % ev["source_id"])
                ro = manifest.get("root_origin", ev["source_id"])
            except Exception:
                ro = ev["source_id"]
        origins.add(ro)
    return len(origins)


def _now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
