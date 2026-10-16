"""Human review: semantic uncertainty lands here instead of being hidden
(PRD §25-26). Actions are ordinary transactions — nothing special-cased.
"""
import glob
import calendar
import json
import os
import time
from typing import Optional

from . import claims, ids
from .transaction import Transaction, TxnError

QUEUE = ".state/review-queue.jsonl"

ACTIONS = (
    "accept", "reject", "merge", "mark_duplicate", "mark_authoritative",
    "mark_superseded", "set_scope", "set_validity", "defer", "reopen",
    "set_topic_kind",
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
    queue = _txn_queue(txn)
    queue.append(item)
    txn.stage_write(QUEUE, _dump(queue))
    return item["id"]


def _dump(queue: list) -> str:
    return "\n".join(json.dumps(x, ensure_ascii=False) for x in queue) + ("\n" if queue else "")


def _txn_queue(txn) -> list:
    """Queue including items already staged by this transaction: staged write
    to QUEUE wins over the on-disk file so multiple add_item calls compose."""
    staged = os.path.join(txn.staging_dir, QUEUE)
    path = staged if os.path.isfile(staged) else txn.wiki.p(QUEUE)
    out = []
    if os.path.isfile(path):
        with open(path) as f:
            for line in f:
                line = line.strip()
                if line:
                    out.append(json.loads(line))
    return out


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
    if action not in ("defer", "reopen", "set_topic_kind"):
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
    elif action == "set_topic_kind":
        from . import deps as deps_mod, topics as topics_mod
        if item.get("kind") != "topic_kind_conflict":
            raise TxnError("set_topic_kind requires a topic_kind_conflict item")
        subj = str(params.get("subject") or "").strip()
        new_kind = str(params.get("kind") or "").strip().lower()
        if subj not in (item.get("affected") or []):
            raise TxnError("subject %r not affected by item %s" % (subj, item_id))
        entry = topics_mod.get(wiki, subj)
        if entry is None or entry.get("status") != "conflict":
            raise TxnError("subject %r is not a declared topic in conflict" % subj)
        if new_kind not in entry.get("alternate_kinds", []) + [entry["kind"]]:
            declared = sorted(set(entry.get("alternate_kinds", [])
                                  + [entry["kind"]]))
            raise TxnError("kind %r was not declared for %r (declared: %s)"
                           % (new_kind, subj, ", ".join(declared)))
        old_dir = topics_mod.PAGE_DIR.get(entry["kind"])
        def _upd(reg):
            e = reg.setdefault("topics", {}).get(subj)
            if e is not None:
                e["kind"] = new_kind
                e["status"] = "resolved"
        txn.stage_state(topics_mod.TOPICS_FILE, _upd)
        from .text import slugify
        slug = slugify(subj)
        if old_dir and old_dir != topics_mod.PAGE_DIR.get(new_kind):
            stale = [os.path.basename(p) for pat in (slug + ".md", slug + "-*.md")
                     for p in glob.glob(wiki.p("wiki", old_dir, pat))]
            if stale:
                deps_mod.mark_stale(
                    txn, ["wiki/%s/%s" % (old_dir, name) for name in stale])
    elif action == "defer":
        pass
    elif action == "reopen":
        if item["status"] != "deferred":
            raise TxnError("reopen requires a deferred item (status=%s)" % item["status"])

    entry = {"action": action, "at": _now()}
    if note:
        entry["note"] = note
    item.setdefault("history", []).append(entry)
    if action == "defer":
        item["status"] = "deferred"
        item["deferred_at"] = _now()
        item.pop("resolved_at", None)
    elif action == "reopen":
        item["status"] = "open"
        item.pop("deferred_at", None)
        item.pop("resolved_at", None)
        item["resolution"] = ""
    else:
        item["status"] = "resolved"
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


def summary(wiki) -> dict:
    """Queue counts: open, deferred, total (consumed by overview/graph views)."""
    items = wiki.read_jsonl(QUEUE)
    return {"open": sum(1 for i in items if i.get("status") == "open"),
            "deferred": sum(1 for i in items if i.get("status") == "deferred"),
            "total": len(items)}


def age_days(ts) -> int:
    """Whole days since an ISO timestamp; 0 when missing/unparseable."""
    try:
        then = calendar.timegm(time.strptime(ts, "%Y-%m-%dT%H:%M:%SZ"))
    except (TypeError, ValueError):
        return 0
    return max(0, int(time.time() - then) // 86400)


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
