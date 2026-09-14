"""Read-only query preparation (PRD §33-36).

Returns matches + conflicts + gaps for the agent to synthesize into an
answer with citations — or to refuse when evidence is insufficient (§35).
This module NEVER writes.
"""
import re
from typing import Optional

from . import claims
from .contextpack import STOPWORDS, _tokens

_WORD_RE = re.compile(r"[a-z0-9_]+")


def prepare(wiki, question: str, as_of: Optional[str] = None) -> dict:
    q_tokens = _tokens(question)
    all_claims = claims.list_claims(wiki)

    matches = []
    conflicts = []
    for c in all_claims:
        hay = _tokens("%s %s %s" % (c["subject"], c["predicate"], c.get("value")))
        overlap = q_tokens & hay
        if not overlap:
            continue

        if as_of:
            # historical: claim must have been valid on that date
            if c.get("valid_from") and c["valid_from"] > as_of:
                continue
            if c.get("valid_to") and c["valid_to"] < as_of:
                continue
        else:
            # current: only still-valid knowledge
            if c["status"] in ("superseded", "rejected"):
                continue
            if c.get("valid_to"):
                continue

        matches.append({
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
            "score": len(overlap),
        })

        if c["status"] == "disputed":
            conflicts.append({
                "claim_id": c["id"],
                "reason": "disputed claim matches the question",
            })

    matches.sort(key=lambda m: (m["score"], m["status"] == "accepted"), reverse=True)
    matches = matches[:10]

    # unresolved review items touching matched subjects
    open_items = [r for r in wiki.read_jsonl(".state/review-queue.jsonl")
                  if r.get("status") == "open"]
    matched_subjects = {m["claim_id"] for m in matches}
    for item in open_items:
        if any(a.split("@")[0] in matched_subjects
               or _tokens(item.get("problem", "")) & q_tokens for a in item.get("affected", [])):
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
        known |= _tokens("%s %s %s" % (c["subject"], c["predicate"], c.get("value")))
    gaps = sorted(t for t in q_tokens if t not in known)

    return {
        "question": question,
        "as_of": as_of,
        "matches": matches,
        "decisions": [d["id"] for d in claims.list_decisions(wiki)][:5],
        "conflicts": conflicts,
        "gaps": gaps,
    }
