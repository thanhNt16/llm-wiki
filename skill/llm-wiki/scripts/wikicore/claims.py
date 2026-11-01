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

INDEX_REL = ".state/claims-index.json"


def norm_key(subject, predicate) -> str:
    """Canonical match key — identical semantics to compile._norm pairing."""
    return "%s/%s" % (str(subject).strip().lower(), str(predicate).strip().lower())


def _index_entry(claim: dict) -> dict:
    return {"key": norm_key(claim.get("subject", ""), claim.get("predicate", "")),
            "status": claim.get("status", ""),
            "subject": claim.get("subject", ""),
            "version": claim.get("version", 1)}


def _store_dir_empty(wiki, store: str, prefix: str) -> bool:
    """True when the store dir holds no '<prefix>*.json' (os.listdir only,
    no file reads)."""
    try:
        names = os.listdir(wiki.p(store))
    except OSError:
        return True
    return not any(n.startswith(prefix) and n.endswith(".json") for n in names)


def _load_index_for_staging(txn: Transaction) -> None:
    """The index is a rebuildable derived cache: an unparseable, non-dict, or
    wrong-shaped on-disk file must never brick a save. Seed the staging
    overlay from {} so stage_state rebuilds instead of re-reading garbage.
    A rebuilt index is complete only when the wiki is truly empty — otherwise
    it may hold a partial view, so it is marked incomplete for readers."""
    if INDEX_REL in txn._state_overlay:
        return
    path = txn.wiki.p(INDEX_REL)
    if os.path.isfile(path):
        try:
            with open(path) as f:
                idx = json.load(f)
        except ValueError:
            idx = None
        if isinstance(idx, dict) and isinstance(idx.get("claims"), dict) \
                and isinstance(idx.get("decisions"), list):
            return  # shape-valid: stage_state re-reads it; "complete" preserved
    txn._state_overlay[INDEX_REL] = {
        "claims": {},
        "decisions": [],
        "complete": _store_dir_empty(txn.wiki, "claims", "claim_")
        and _store_dir_empty(txn.wiki, "decisions", "decision_"),
    }


def _stage_index_update(txn: Transaction, claim: dict) -> None:
    """Keep the derived claims index in the same transaction as the claim write."""
    entry = _index_entry(claim)
    _load_index_for_staging(txn)

    def _upd(idx: dict) -> None:
        idx.setdefault("claims", {})
        idx["claims"][claim["id"]] = entry

    txn.stage_state(INDEX_REL, _upd)


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
    # prefer this txn's staged copy: without it a second save in the same
    # transaction re-reads live-disk bytes — bumping version twice and
    # letting terminal status revert (T6-B1)
    staged_path = os.path.join(txn.staging_dir, _claim_path(cid))
    if os.path.isfile(staged_path):
        with open(staged_path) as f:
            existing = json.load(f)
        from_staging = True
    else:
        from_staging = False
        path = txn.wiki.p(_claim_path(cid))
        if os.path.isfile(path):
            with open(path) as f:
                existing = json.load(f)
    if existing:
        if not from_staging:
            claim["version"] = existing["version"] + 1
        else:
            claim["version"] = existing["version"]  # bump already applied
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
    _stage_index_update(txn, claim)
    txn.stage_write(_claim_path(cid), claim)
    return claim


def load_index(wiki):
    """(index, fresh). fresh iff the watermark equals the current revision
    AND the index is marked complete. Missing/corrupt/wrong-shaped file
    yields (None, False)."""
    try:
        idx = wiki.load_json(INDEX_REL)
    except (OSError, ValueError):
        return None, False
    if not isinstance(idx, dict) or not isinstance(idx.get("claims"), dict) \
            or not isinstance(idx.get("decisions"), list):
        return None, False
    return idx, idx.get("revision") == wiki.revision() and idx.get("complete") is True


def _scan_claims(wiki, status=None, subject=None) -> list:
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


def _dir_ids(wiki, rel: str, prefix: str):
    """Ids of on-disk objects under rel (readdir only, no reads); None if
    the directory does not exist yet."""
    d = wiki.p(rel)
    if not os.path.isdir(d):
        return None
    return {n[:-5] for n in os.listdir(d)
            if n.startswith(prefix) and n.endswith(".json")}


def list_claims(wiki, status: Optional[str] = None, subject: Optional[str] = None) -> list:
    if status or subject:
        idx, fresh = load_index(wiki)
        if fresh:
            disk = _dir_ids(wiki, "claims", "claim_")
            if disk is not None and not disk.issubset(idx["claims"]):
                # fresh index that misses an on-disk claim (out-of-band
                # write, git merge, tamper) is lying — scan, never wrong
                return _scan_claims(wiki, status, subject)
            out = []
            for cid in sorted(idx["claims"]):
                meta = idx["claims"][cid]
                if status and meta.get("status") != status:
                    continue
                if subject and meta.get("subject") != subject:
                    continue
                try:
                    out.append(load_claim(wiki, cid))
                except (OSError, json.JSONDecodeError, TxnError):
                    # index lied about a live claim — fall back, never wrong
                    return _scan_claims(wiki, status, subject)
            return out
    return _scan_claims(wiki, status, subject)


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
    # prefer this txn's staged copy: without it a second save in the same
    # transaction re-reads live-disk bytes — same gap save_claim had
    # (SP3-F3); keeps bump-once-per-commit and staged recorded_at (SP3-T10)
    existing = None
    from_staging = False
    staged_path = os.path.join(txn.staging_dir, _decision_path(did))
    if os.path.isfile(staged_path):
        with open(staged_path) as f:
            existing = json.load(f)
        from_staging = True
    else:
        path = txn.wiki.p(_decision_path(did))
        if os.path.isfile(path):
            with open(path) as f:
                existing = json.load(f)
    if existing:
        if not from_staging:
            decision["version"] = existing["version"] + 1
        else:
            decision["version"] = existing["version"]  # bump already applied
        decision["recorded_at"] = existing["recorded_at"]
    else:
        # creation: callers may not inject a version (same rule as save_claim)
        decision["version"] = 1
        decision.setdefault("recorded_at", _now())
    errors = schema_validate(decision, DECISION_SCHEMA)
    if errors:
        raise TxnError("decision %s invalid: %s" % (did, "; ".join(errors)))
    _load_index_for_staging(txn)
    def _upd_dec(idx: dict) -> None:
        idx.setdefault("decisions", [])
        if did not in idx["decisions"]:
            idx["decisions"].append(did)

    txn.stage_state(INDEX_REL, _upd_dec)
    txn.stage_write(_decision_path(did), decision)
    return decision


def _scan_decisions(wiki) -> list:
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


def list_decisions(wiki) -> list:
    idx, fresh = load_index(wiki)
    if fresh and isinstance(idx.get("decisions"), list):
        disk = _dir_ids(wiki, "decisions", "decision_")
        if disk is not None and not disk.issubset(idx["decisions"]):
            # fresh index missing an on-disk decision — scan, never wrong
            return _scan_decisions(wiki)
        out = []
        for did in sorted(idx["decisions"]):
            try:
                out.append(load_decision(wiki, did))
            except (OSError, json.JSONDecodeError, TxnError):
                return _scan_decisions(wiki)
        return out
    return _scan_decisions(wiki)


def rebuild_index(wiki) -> dict:
    """Regenerate the derived index from the authoritative objects (doctor path).
    Committed through a Transaction so the commit hook stamps the post-commit
    revision watermark; a full scan means the result is always complete."""
    # capture the base revision BEFORE the scan: a concurrent commit landing
    # between scan and commit must fail the optimistic check (ConflictError),
    # not slip through and get stamped onto an index missing its claims
    base = wiki.revision()
    idx = {"claims": {}, "decisions": [], "complete": True}
    for c in _scan_claims(wiki):
        idx["claims"][c["id"]] = _index_entry(c)
    idx["decisions"] = sorted(d["id"] for d in _scan_decisions(wiki))
    txn = Transaction(wiki, "index-rebuild")
    txn.stage_write(INDEX_REL, idx)
    txn.commit(base)
    return wiki.load_json(INDEX_REL)
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
