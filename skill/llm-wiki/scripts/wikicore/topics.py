"""Canonical topic registry (.llm-wiki/topics.json).

Declared by the agent during the compile extract phase (topics.jsonl),
merged deterministically at apply time inside the claim transaction.
Drives declared-kind routing and single-claim page eligibility in
synthesize.page_targets. Never hand-edit.
"""
import json
import os

from .text import slugify  # noqa: F401  (re-exported for page-targets)
from .transaction import Transaction, TxnError

TOPICS_FILE = "topics.json"

KINDS = ("concept", "entity", "procedure", "question", "change",
         "decision", "source")
_PLURAL = {"concepts": "concept", "entities": "entity",
           "procedures": "procedure", "questions": "question",
           "changes": "change", "decisions": "decision",
           "sources": "source"}
PAGE_DIR = {"concept": "concepts", "entity": "entities",
            "procedure": "procedures", "question": "questions",
            "change": "changes", "source": "sources"}


def _norm_kind(k: str) -> str:
    k = str(k).strip().lower()
    if k in _PLURAL:
        return _PLURAL[k]
    return k


def _entry(kind, title, declared_by):
    return {"kind": kind, "title": title, "status": "resolved",
            "aliases": [], "alternate_kinds": [],
            "declared_by": [dict(declared_by)]}


def load(wiki) -> dict:
    if not os.path.isfile(wiki.p(TOPICS_FILE)):
        return {"version": 0, "topics": {}}
    with open(wiki.p(TOPICS_FILE)) as f:
        return json.load(f)


def get(wiki, subject):
    return load(wiki)["topics"].get(subject)


def get_kind(wiki, subject):
    e = get(wiki, subject)
    if e and e.get("status") == "resolved" and e.get("kind") in PAGE_DIR:
        return PAGE_DIR[e["kind"]]
    return None


def _load_for_txn(wiki, txn):
    """Registry incl. this txn's staged overlay."""
    data = txn._state_overlay.get(TOPICS_FILE)
    if data is None:
        data = load(wiki)
    return data


def _stage(txn, reg):
    txn.stage_state(TOPICS_FILE, lambda data: data.update(reg))


def _title_for(subject):
    return subject.rsplit(".", 1)[-1].replace("-", " ").title()


def _merge_one(reg, row, declared_by, out):
    subj = str(row["subject"]).strip()
    kind = _norm_kind(row["kind"])
    ent = reg["topics"].get(subj)
    if ent is None:
        ent = _entry(kind, str(row["title"]).strip(), declared_by)
        reg["topics"][subj] = ent
        out["registered"] += 1
    else:
        db = dict(declared_by)
        if db not in ent.setdefault("declared_by", []):
            ent["declared_by"].append(db)
        if ent["kind"] != kind:
            if kind not in ent.setdefault("alternate_kinds", []):
                ent["alternate_kinds"].append(kind)
            ent["status"] = "conflict"
            if subj not in out["kind_conflicts"]:
                out["kind_conflicts"].append(subj)
            return
    for a in row.get("aliases") or []:
        a = str(a).strip()
        if a and a not in ent.setdefault("aliases", []):
            ent["aliases"].append(a)
    new_title = str(row.get("title") or "").strip()
    if new_title and new_title != ent["title"]:
        out["warnings"].append("topic %r: title %r kept over %r"
                               % (subj, ent["title"], new_title))


def merge_rows(txn: Transaction, rows: list, declared_by: dict) -> dict:
    """Merge declared topics.jsonl rows into the registry (staged)."""
    from . import review
    reg = _load_for_txn(txn.wiki, txn)
    reg.setdefault("topics", {})
    out = {"declared": len(rows), "registered": 0,
           "kind_conflicts": [], "warnings": []}
    for row in rows:
        subj = str(row["subject"]).strip()
        before = len(out["kind_conflicts"])
        _merge_one(reg, row, declared_by, out)
        if len(out["kind_conflicts"]) > before:
            review.add_item(
                txn, "topic_kind_conflict",
                "subject %r declared with conflicting kinds" % subj,
                [dict(declared_by)], [subj],
                "wrong kind routes the page to the wrong directory",
                proposed=reg["topics"][subj]["kind"])
    if rows:
        reg["version"] = int(reg.get("version", 0)) + 1
        _stage(txn, reg)
    return out


def ensure_source_topic(txn: Transaction, subject: str, title: str,
                        declared_by: dict) -> None:
    reg = _load_for_txn(txn.wiki, txn)
    reg.setdefault("topics", {})
    if subject not in reg["topics"]:
        ent = _entry("source", title, declared_by)
        ent["auto_registered"] = True
        reg["topics"][subject] = ent
        reg["version"] = int(reg.get("version", 0)) + 1
        _stage(txn, reg)


def auto_register(txn: Transaction, subjects: list, declared_by: dict) -> dict:
    """Register undeclared candidate subjects as kind=concept."""
    reg = _load_for_txn(txn.wiki, txn)
    reg.setdefault("topics", {})
    added = []
    for s in subjects:
        if s in reg["topics"]:
            continue
        ent = _entry("concept", _title_for(s), declared_by)
        ent["auto_registered"] = True
        reg["topics"][s] = ent
        added.append(s)
    if added:
        reg["version"] = int(reg.get("version", 0)) + 1
        _stage(txn, reg)
    return {"auto_registered": sorted(added)}


def unclaimed(wiki) -> list:
    """Declared registry subjects with zero claims."""
    from . import claims
    have = {c["subject"] for c in claims.list_claims(wiki)}
    return sorted(s for s, e in load(wiki)["topics"].items()
                  if e.get("kind") not in ("source", "decision")
                  and s not in have)


def parse_topics_file(path: str):
    """Parse topics.jsonl -> (rows, declared_subjects). TxnError w/ line nos."""
    rows = []
    declared = set()
    with open(path, encoding="utf-8") as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError as e:
                raise TxnError("topics.jsonl line %d: invalid JSON: %s" % (i, e))
            if not isinstance(row, dict):
                raise TxnError("topics.jsonl line %d: row must be an object" % i)
            for field in ("subject", "kind", "title"):
                if not str(row.get(field) or "").strip():
                    raise TxnError("topics.jsonl line %d: missing %r"
                                   % (i, field))
            k = _norm_kind(row["kind"])
            if k not in KINDS or k == "source":
                raise TxnError("topics.jsonl line %d: bad kind %r (valid: %s)"
                               % (i, row["kind"],
                                  ", ".join(x for x in KINDS if x != "source")))
            row["kind"] = k
            rows.append(row)
            declared.add(row["subject"].strip())
    return rows, declared


def backfill(wiki) -> dict:
    """Register every existing claim subject (idempotent; never overwrites)."""
    from . import claims, synthesize
    reg = load(wiki)
    reg.setdefault("topics", {})
    warnings = []
    rules, default_kind = synthesize._load_kind_rules(wiki, warnings)
    groups = {}
    for c in claims.list_claims(wiki):
        groups.setdefault(c["subject"], []).append(c)
    added = 0
    declared_by = {"source_id": "backfill", "version": 0}
    for subj in sorted(groups):
        if subj in reg["topics"]:
            continue
        plural, _ = synthesize._route_kind(subj, groups[subj], rules,
                                           default_kind)
        k = _PLURAL.get(plural, "concept")
        ent = _entry(k, _title_for(subj), declared_by)
        ent["auto_registered"] = True
        reg["topics"][subj] = ent
        added += 1
    if added:
        reg["version"] = int(reg.get("version", 0)) + 1
        txn = Transaction(wiki, "wiki-topics-backfill")
        txn.stage_state(TOPICS_FILE, lambda data: data.update(reg))
        txn.changes = {"topics_registered": added}
        txn.commit(wiki.revision())
    return {"topics_registered": added, "warnings": warnings}
