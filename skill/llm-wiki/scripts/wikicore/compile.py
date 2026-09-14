"""Compile pipeline: pending evidence -> candidate claims -> reconciliation
-> accepted knowledge -> dependency invalidation (PRD §30, §16).

Deterministic parts (matching, policy application, supersession, persistence)
live here; the agent classifies relationships between script calls per
references/RECONCILIATION.md.
"""
import json
import os

from . import claims, deps, ids
from .transaction import Transaction, TxnError

RELATIONSHIPS = (
    "CORRECTION", "POLICY_CHANGE", "SCOPE_DIFFERENCE", "CONTRADICTION",
    "CORROBORATION", "DUPLICATE", "UNRELATED",
)


def compile_plan(wiki) -> dict:
    from . import sources as sources_mod

    return {"pending": sources_mod.pending_versions(wiki)}


def _run_dir(wiki, run_id: str) -> str:
    path = wiki.p(".state", "staging", run_id)
    if not os.path.isdir(path):
        raise TxnError("unknown compile run: %s" % run_id)
    return path


def _load_json(path):
    with open(path) as f:
        return json.load(f)


def stage_candidates(wiki, source_id: str, source_version: int,
                     candidates: list, report: dict) -> str:
    """Stage agent-extracted candidates. Staging only — nothing canonical yet."""
    from . import sources as sources_mod

    src_manifest = sources_mod.get_manifest(wiki, source_id)
    if src_manifest is None:
        raise TxnError("unknown source: %s" % source_id)
    txn = Transaction(wiki, "wiki-stage-candidates")
    assigned = []
    for c in candidates:
        if not ids.validate(c.get("id", "")) or c.get("id") == "auto":
            c["id"] = ids.new("claim")
        if not c.get("root_origin"):
            c["root_origin"] = src_manifest.get("root_origin") or source_id
        c["subject"] = str(c["subject"]).strip()
        c["predicate"] = str(c["predicate"]).strip()
        assigned.append(c)
    payload = {
        "source_id": source_id,
        "source_version": source_version,
        "candidates": assigned,
        "report": report,
    }
    path = os.path.join(txn.staging_dir, "compile.json")
    with open(path, "w") as f:
        json.dump(payload, f, indent=2, ensure_ascii=False)
    # keep staging dir for the next phase instead of committing
    return txn.run_id


def _norm(s) -> str:
    return str(s).strip().lower()


def _scopes_equal(a, b) -> bool:
    return {k: str(v) for k, v in (a or {}).items()} == {k: str(v) for k, v in (b or {}).items()}


def _temporal(old, new) -> str:
    if old.get("valid_to") and new.get("valid_from") and old["valid_to"] < new["valid_from"]:
        return "disjoint"
    return "overlapping"


def reconcile_prepare(wiki, run_id: str) -> dict:
    payload = _load_json(os.path.join(_run_dir(wiki, run_id), "compile.json"))
    existing = [c for c in claims.list_claims(wiki) if c["status"] not in ("rejected", "superseded")]
    comparisons = []
    for i, cand in enumerate(payload["candidates"]):
        key = "%s/%s" % (_norm(cand["subject"]), _norm(cand["predicate"]))
        matches = []
        for ex in existing:
            ex_key = "%s/%s" % (_norm(ex["subject"]), _norm(ex["predicate"]))
            if ex_key != key:
                continue
            matches.append({
                "claim_id": ex["id"],
                "same_scope": _scopes_equal(ex.get("scope"), cand.get("scope")),
                "same_value": ex.get("value") == cand.get("value"),
                "temporal": _temporal(ex, cand),
                "status": ex["status"],
                "current_value": ex.get("value"),
            })
        comparisons.append({"index": i, "candidate": cand, "matches": matches})
    result = {"run_id": run_id, "comparisons": comparisons}
    with open(os.path.join(_run_dir(wiki, run_id), "comparisons.json"), "w") as f:
        json.dump(result, f, indent=2, ensure_ascii=False)
    return result


def _append_review_item(txn: Transaction, kind: str, problem: str, evidence: list,
                        affected: list, risk: str, proposed: str = "") -> str:
    item = {
        "id": ids.new("review"),
        "created_at": _now(),
        "kind": kind,
        "status": "open",
        "problem": problem,
        "evidence": evidence,
        "affected": affected,
        "risk": risk,
        "proposed_resolution": proposed,
    }
    queue = txn.wiki.read_jsonl(".state/review-queue.jsonl")
    queue.append(item)
    txn.stage_write(".state/review-queue.jsonl",
                    "\n".join(json.dumps(x, ensure_ascii=False) for x in queue) + ("\n" if queue else ""))
    return item["id"]


def _from_candidate(wiki, cand: dict, classification: dict, status: str,
                    acceptance_reason: str) -> dict:
    claim = {
        "id": cand["id"],
        "subject": cand["subject"],
        "predicate": cand["predicate"],
        "value": cand.get("value"),
        "scope": cand.get("scope") or {},
        "valid_from": classification.get("valid_from") or cand.get("valid_from"),
        "valid_to": classification.get("valid_to") or cand.get("valid_to") or None,
        "recorded_at": cand.get("recorded_at") or _now(),
        "status": status,
        "authority": classification.get("authority") or cand.get("authority"),
        "evidence": cand.get("evidence", []),
        "supersedes": list(cand.get("supersedes", [])),
        "root_origin": cand.get("root_origin"),
        "version": 1,
    }
    if status == "accepted":
        claim["acceptance"] = {"reason": acceptance_reason, "accepted_at": _now()}
    return claim


def _auto_status(wiki, cand: dict, classification: dict):
    cfg = wiki.load_config()
    policy = cfg["acceptance_policy"]
    authority = (classification.get("authority") or cand.get("authority") or {})
    atype = authority.get("type", "agent_inference")
    if atype in policy["auto_accept_authority"]:
        return "accepted", atype
    return "candidate", ""


def _append_evidence(target: dict, cand: dict) -> bool:
    existing = {
        json_key(ev) for ev in target.get("evidence", [])
    }
    added = False
    for ev in cand.get("evidence", []):
        if json_key(ev) not in existing:
            target.setdefault("evidence", []).append(ev)
            added = True
    return added


def json_key(obj) -> str:
    return json.dumps(obj, sort_keys=True, ensure_ascii=False)


def reconcile_apply(wiki, run_id: str, classifications: list, base_revision: int) -> dict:
    rundir = _run_dir(wiki, run_id)
    payload = _load_json(os.path.join(rundir, "compile.json"))
    candidates = payload["candidates"]
    source_id = payload["source_id"]
    source_version = payload["source_version"]

    txn = Transaction(wiki, "wiki-compile")
    changes = {"claims_created": 0, "claims_updated": 0, "claims_superseded": 0,
               "review_items": 0}
    conflicts = []

    for cls in classifications:
        if cls.get("relationship") not in RELATIONSHIPS:
            raise TxnError("bad relationship: %r" % cls.get("relationship"))
        cand = candidates[cls["index"]]
        rel = cls["relationship"]
        target = None
        if cls.get("target_claim_id"):
            target = claims.load_claim(wiki, cls["target_claim_id"])

        if rel == "UNRELATED":
            status, reason = _auto_status(wiki, cand, cls)
            claims.save_claim(txn, _from_candidate(wiki, cand, cls, status, reason))
            changes["claims_created"] += 1
            deps.invalidate(wiki, [cand["id"]], txn=txn)

        elif rel == "SCOPE_DIFFERENCE":
            status, reason = _auto_status(wiki, cand, cls)
            claims.save_claim(txn, _from_candidate(wiki, cand, cls, status, reason))
            changes["claims_created"] += 1
            deps.invalidate(wiki, [cand["id"]], txn=txn)

        elif rel == "DUPLICATE":
            if _append_evidence(target, cand):
                claims.save_claim(txn, target)
                changes["claims_updated"] += 1
            origins = {ev.get("root_origin") or ev["source_id"] for ev in target["evidence"]}
            if len(origins) > 1:
                rid = _append_review_item(
                    txn, "duplicate_identity",
                    "claim %s now has evidence from multiple origins" % target["id"],
                    [target["id"]], [target["id"]], "low",
                    "confirm these are copies, not independent sources")
                changes["review_items"] += 1
                conflicts.append(rid)

        elif rel == "CORROBORATION":
            _append_evidence(target, cand)
            indep = claims.independence(wiki, target)
            cfg = wiki.load_config()
            threshold = cfg["acceptance_policy"]["corroboration_auto_accept"]
            if target["status"] in ("candidate", "provisional") and indep >= threshold:
                target["status"] = "accepted"
                target["acceptance"] = {"reason": "independent_corroboration",
                                        "accepted_at": _now()}
            claims.save_claim(txn, target)
            changes["claims_updated"] += 1

        elif rel in ("CORRECTION", "POLICY_CHANGE"):
            status, reason = _auto_status(wiki, cand, cls)
            new_claim = _from_candidate(wiki, cand, cls, status, reason)
            new_claim["supersedes"].append(target["id"])
            claims.save_claim(txn, new_claim)
            old = claims.supersede(txn, target, new_claim["id"],
                                   policy_change=(rel == "POLICY_CHANGE"),
                                   new_valid_from=cls.get("valid_from"))
            claims.save_claim(txn, old)
            changes["claims_created"] += 1
            changes["claims_superseded"] += 1
            deps.invalidate(wiki, [target["id"], new_claim["id"]], txn=txn)

        elif rel == "CONTRADICTION":
            new_claim = _from_candidate(wiki, cand, cls, "disputed", "")
            claims.save_claim(txn, new_claim)
            changes["claims_created"] += 1
            rid = _append_review_item(
                txn, "possible_contradiction",
                "%s/%s conflicts with existing claim %s" % (
                    cand["subject"], cand["predicate"], target["id"]),
                [target["id"], new_claim["id"]],
                [target["id"], new_claim["id"]], "high",
                "decide which value is correct or whether scopes differ")
            changes["review_items"] += 1
            conflicts.append(rid)
            deps.invalidate(wiki, [new_claim["id"]], txn=txn)

    # persist extraction for this version and mark it compiled
    extraction = {
        "source_version": source_version,
        "extraction_report": payload["report"],
        "candidates": candidates,
        "compiled": True,
    }
    txn.stage_write("sources/%s/extraction.json" % source_id, extraction)

    def _mark(state):
        state.setdefault("compiled", {}).setdefault(source_id, [])
        if source_version not in state["compiled"][source_id]:
            state["compiled"][source_id].append(source_version)

    txn.stage_state(".state/manifest.json", _mark)

    txn.changes = changes
    txn.conflicts = conflicts
    receipt = txn.commit(base_revision)
    # staging compile.json is consumed
    compile_leftover = os.path.join(rundir, "compile.json")
    if os.path.isfile(compile_leftover):
        os.unlink(compile_leftover)
    return receipt


def _now() -> str:
    import time

    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
