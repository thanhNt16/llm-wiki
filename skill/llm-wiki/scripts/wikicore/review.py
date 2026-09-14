"""Human review: semantic uncertainty lands here instead of being hidden
(PRD §25-26). Actions are ordinary transactions — nothing special-cased.
"""
import json
import os
from typing import Optional

from . import claims, ids
from .transaction import Transaction, TxnError

QUEUE = ".state/review-queue.jsonl"

ACTIONS = (
    "accept", "reject", "merge", "mark_duplicate", "mark_authoritative",
    "mark_superseded", "set_scope", "set_validity", "defer",
)


def add_item(txn: Transaction, kind: str, problem: str, evidence: list,
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
    queue = txn.wiki.read_jsonl(QUEUE)
    queue.append(item)
    txn.stage_write(QUEUE, _dump(queue))
    return item["id"]


def _dump(queue: list) -> str:
    return "\n".join(json.dumps(x, ensure_ascii=False) for x in queue) + ("\n" if queue else "")


def list_items(wiki, kind: Optional[str] = None, status: str = "open") -> list:
    items = wiki.read_jsonl(QUEUE)
    out = []
    for item in items:
        if kind and item.get("kind") != kind:
            continue
        if status and item.get("status") != status:
            continue
        out.append(item)
    return out


def show(wiki, item_id: str) -> dict:
    for item in wiki.read_jsonl(QUEUE):
        if item["id"] == item_id:
            return item
    raise TxnError("unknown review item: %s" % item_id)


def act(wiki, item_id: str, action: str, params: dict, base_revision: int) -> dict:
    if action not in ACTIONS:
        raise TxnError("unknown review action: %r (valid: %s)" % (action, ", ".join(ACTIONS)))
    params = params or {}
    item = show(wiki, item_id)

    txn = Transaction(wiki, "wiki-review")
    claim = None
    if action != "defer":
        cid = params.get("claim_id")
        if not cid:
            raise TxnError("action %r requires claim_id" % action)
        claim = claims.load_claim(wiki, cid)

    note = params.get("note", "")

    if action == "accept":
        claim["status"] = "accepted"
        claim["acceptance"] = {"reason": "human_review", "accepted_at": _now()}
        claims.save_claim(txn, claim)
    elif action == "reject":
        claim["status"] = "rejected"
        claim["acceptance"] = {"reason": "human_review", "accepted_at": _now()}
        claims.save_claim(txn, claim)
    elif action in ("merge", "mark_duplicate"):
        merge_into = params.get("merge_into")
        if not merge_into:
            raise TxnError("action %r requires merge_into" % action)
        target = claims.load_claim(wiki, merge_into)
        from .compile import _append_evidence

        _append_evidence(target, claim)
        claims.save_claim(txn, target)
        claim["status"] = "rejected"
        claim["acceptance"] = {"reason": "duplicate_of:%s" % merge_into, "accepted_at": _now()}
        claims.save_claim(txn, claim)
    elif action == "mark_authoritative":
        claim["authority"] = {"type": "explicit_project_decision",
                              "source": "human_review:" + item_id}
        if claim["status"] != "accepted":
            claim["status"] = "accepted"
            claim["acceptance"] = {"reason": "human_review", "accepted_at": _now()}
        claims.save_claim(txn, claim)
    elif action == "mark_superseded":
        target_id = params.get("target")
        if not target_id:
            raise TxnError("mark_superseded requires target claim_id")
        claims.supersede(txn, claim, target_id, policy_change=False, new_valid_from=None)
        claim["supersedes"] = list(set(claim.get("supersedes", []) + [target_id]))
        claims.save_claim(txn, claim)
    elif action == "set_scope":
        scope = params.get("scope")
        if not isinstance(scope, dict):
            raise TxnError("set_scope requires scope dict")
        claim["scope"] = scope
        claims.save_claim(txn, claim)
    elif action == "set_validity":
        claim["valid_from"] = params.get("valid_from", claim.get("valid_from"))
        claim["valid_to"] = params.get("valid_to", claim.get("valid_to"))
        claims.save_claim(txn, claim)
    elif action == "defer":
        pass

    item["status"] = "deferred" if action == "defer" else "resolved"
    item["resolution"] = "%s%s" % (action, (": " + note) if note else "")
    item["resolved_at"] = _now()
    queue = wiki.read_jsonl(QUEUE)
    for i, existing in enumerate(queue):
        if existing["id"] == item_id:
            queue[i] = item
            break
    txn.stage_write(QUEUE, _dump(queue))

    # release gate: claim mutations invalidate dependent artifacts (PRD §58)
    if claim is not None:
        from . import deps as deps_mod

        deps_mod.invalidate(wiki, [claim["id"]], txn=txn)

    txn.changes = {"review_actions": 1}
    receipt = txn.commit(base_revision)
    receipt["item"] = item
    return receipt


def _now() -> str:
    import time

    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
