"""Read-only query preparation (PRD §33-36).

Returns matches + conflicts + gaps for the agent to synthesize into an
answer with citations — or to refuse when evidence is insufficient (§35).
This module NEVER writes.
"""
import os
from typing import Optional

from . import claims, synthesize
from .contextpack import STOPWORDS
from .text import norm_tokens

_STATUS_WEIGHT = {"accepted": 3, "provisional": 2, "disputed": 1}
_MATCH_LIMIT = 10
_SUPERSEDED_LIMIT = 5
_SUBJECTS_LIMIT = 100
_PAGE_BOUND = 20


def _score(claim: dict, q_tokens: set):
    """Weighted overlap: subject 3, predicate 2, value 1, plus status weight
    (accepted 3 / provisional 2 / disputed 1), evidence (capped at 3) and a
    +1 scope match bonus. Returns (total, breakdown)."""
    s = len(q_tokens & norm_tokens(claim["subject"]))
    p = len(q_tokens & norm_tokens(claim["predicate"]))
    v = len(q_tokens & norm_tokens(str(claim.get("value", ""))))
    scope = norm_tokens(" ".join("%s %s" % kv
                                 for kv in sorted((claim.get("scope") or {}).items())))
    bonus = 1 if q_tokens & scope else 0
    ev = min(len(claim.get("evidence") or []), 3)
    weight = _STATUS_WEIGHT.get(claim["status"], 0)
    return (3 * s + 2 * p + v + weight + ev + bonus,
            {"subject": s, "predicate": p, "value": v,
             "status": weight, "evidence": ev, "scope": bonus})


def prepare(wiki, question: str, as_of: Optional[str] = None) -> dict:
    q_tokens = norm_tokens(question) - STOPWORDS
    all_claims = claims.list_claims(wiki)
    subject_of = {c["id"]: c["subject"] for c in all_claims}

    matches = []
    conflicts = []
    excluded = []
    for c in all_claims:
        score, parts = _score(c, q_tokens)
        if not (parts["subject"] or parts["predicate"] or parts["value"]):
            continue

        if as_of:
            # historical: claim must have been valid on that date
            if c.get("valid_from") and c["valid_from"] > as_of:
                continue
            if c.get("valid_to") and c["valid_to"] < as_of:
                continue
        else:
            # current: only still-valid knowledge; token-matched terminal
            # claims surface separately as `superseded` (stale-knowledge gap)
            if c["status"] in ("superseded", "rejected"):
                excluded.append({
                    "claim_id": c["id"],
                    "subject": c["subject"],
                    "predicate": c["predicate"],
                    "value": c.get("value"),
                    "status": c["status"],
                    "replaced_by": c.get("superseded_by"),
                })
                continue
            if c.get("valid_to"):
                continue

        entry = {
            "claim_id": c["id"],
            "subject": c["subject"],
            "predicate": c["predicate"],
            "value": c.get("value"),
            "unit": c.get("unit"),
            "scope": c.get("scope") or {},
            "status": c["status"],
            "valid_from": c.get("valid_from"),
            "valid_to": c.get("valid_to"),
            "authority": c.get("authority"),
            "evidence": c.get("evidence", []),
            "score": score,
            "score_breakdown": parts,
        }
        if c.get("superseded_by"):
            entry["superseded_by"] = c["superseded_by"]
        matches.append(entry)

        if c["status"] == "disputed":
            conflicts.append({
                "claim_id": c["id"],
                "reason": "disputed claim matches the question",
            })

    matches.sort(key=lambda m: m["score"], reverse=True)
    matches = matches[:_MATCH_LIMIT]

    # unresolved review items touching the matched claims (C6): an item
    # surfaces when affected claim ids match, when affected claims are
    # siblings of a matched claim (same subject), or — only as a fallback —
    # when its problem text overlaps the question tokens.
    open_items = [r for r in wiki.read_jsonl(".state/review-queue.jsonl")
                  if r.get("status") == "open"]
    matched_ids = {m["claim_id"] for m in matches}
    matched_subjects = {m["subject"] for m in matches}
    for item in open_items:
        hit = False
        for a in item.get("affected", []):
            cid = a.split("@")[0]
            if cid in matched_ids or subject_of.get(cid) in matched_subjects:
                hit = True
                break
        if not hit and norm_tokens(item.get("problem", "")) & q_tokens:
            hit = True
        if hit:
            conflicts.append({"review_id": item["id"], "reason": item["problem"]})

    # accepted claims that contradict each other (same subject+predicate+scope,
    # different values, overlapping validity) must surface as conflicts (PRD §34)
    seen_pairs = set()
    for i, m1 in enumerate(matches):
        for m2 in matches[i + 1:]:
            if m1["value"] == m2["value"]:
                continue
            key = (m1["claim_id"], m2["claim_id"])
            if key in seen_pairs or (key[1], key[0]) in seen_pairs:
                continue
            if (m1["subject"], m1["predicate"]) != (m2["subject"], m2["predicate"]):
                continue
            if m1["scope"] != m2["scope"]:
                continue  # different scope is NOT a contradiction (PRD §6)
            if m1["valid_from"] and m2["valid_to"] and m1["valid_from"] > m2["valid_to"]:
                continue
            if m2["valid_from"] and m1["valid_to"] and m2["valid_from"] > m1["valid_to"]:
                continue
            seen_pairs.add(key)
            conflicts.append({
                "claims": [m1["claim_id"], m2["claim_id"]],
                "reason": "same subject/predicate/scope with different values",
            })

    # knowledge gaps: content words with no coverage anywhere
    known = set()
    for c in all_claims:
        known |= norm_tokens("%s %s %s" % (c["subject"], c["predicate"], c.get("value")))
    gaps = sorted(t for t in q_tokens if t not in known)
    if excluded and not matches:
        gaps.append("stale_knowledge")  # only superseded knowledge answered

    return {
        "question": question,
        "as_of": as_of,
        "matches": matches,
        "superseded": excluded[:_SUPERSEDED_LIMIT],
        "decisions": [d["id"] for d in claims.list_decisions(wiki)][:5],
        "conflicts": conflicts,
        "gaps": gaps,
    }


def subjects(wiki, prefix: Optional[str] = None) -> list:
    """Read-only subject rollup for `wiki.py subjects` (Q2): predicates,
    status counts, distinct evidence sources and stale pages per subject.
    Sorted by subject; bounded output."""
    grouped = {}
    claim_subject = {}
    for c in claims.list_claims(wiki):
        grouped.setdefault(c["subject"], []).append(c)
        claim_subject[c["id"]] = c["subject"]

    stale_pages = {}
    for kind in synthesize.PAGE_KINDS:
        pdir = wiki.p("wiki", kind)
        if not os.path.isdir(pdir):
            continue
        for name in sorted(os.listdir(pdir)):
            if not name.endswith(".md") or name == "index.md":
                continue
            rel = "wiki/%s/%s" % (kind, name)
            try:
                with open(wiki.p(rel)) as f:
                    front, _ = synthesize._split_front(f.read())
            except OSError:
                continue
            if not front.get("stale"):
                continue
            subject = front.get("subject")
            if subject not in grouped:
                subject = next((claim_subject.get(d.split("@", 1)[0])
                                for d in front.get("deps") or []
                                if d.split("@", 1)[0] in claim_subject), None)
            if subject in grouped:
                stale_pages.setdefault(subject, []).append(rel)

    out = []
    for subject in sorted(grouped):
        if prefix and not subject.startswith(prefix):
            continue
        cs = grouped[subject]
        statuses = {}
        for c in cs:
            statuses[c["status"]] = statuses.get(c["status"], 0) + 1
        out.append({
            "subject": subject,
            "predicates": sorted({c["predicate"] for c in cs}),
            "statuses": statuses,
            "claim_count": len(cs),
            "source_count": len({ev["source_id"] for c in cs
                                 for ev in c.get("evidence", [])
                                 if ev.get("source_id")}),
            "stale_pages": sorted(stale_pages.get(subject, []))[:_PAGE_BOUND],
        })
        if len(out) >= _SUBJECTS_LIMIT:
            break
    return out
