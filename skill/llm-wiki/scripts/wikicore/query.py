"""Read-only query preparation (PRD §33-36).

Returns matches + conflicts + gaps for the agent to synthesize into an
answer with citations — or to refuse when evidence is insufficient (§35).
This module NEVER writes.
"""
import math
import os
import time
from collections import Counter
from typing import Optional

from . import claims, pages, synthesize
from .contextpack import STOPWORDS
from .text import _SPLIT_RE, norm_tokens

# BM25-lite retrieval (F1 paraphrase / F2 predicate-IDF): per-field Robertson
# IDF over the whole claim corpus down-weights corpus-common predicates like
# `status`, while the subject-alias index lets "stc-108" / "stc 108" reach
# claims filed under the `stc-108` subject.
_K1 = 1.2
_B = 0.75
_FIELD_WEIGHTS = {"subject": 3.0, "predicate": 2.0, "value": 1.0}
_MATCH_LIMIT = 10
_SUPERSEDED_LIMIT = 5
_SUBJECTS_LIMIT = 100
_PAGE_BOUND = 20


def _text(v) -> str:
    """Coerce a possibly-missing field to a tokenizable string."""
    return "" if v is None else str(v)


def _norm_list(s: str) -> list:
    """norm_tokens with multiplicity preserved (term frequencies)."""
    out = []
    for piece in _SPLIT_RE.split(s.lower()):
        if len(piece) < 3:
            continue
        out.extend(norm_tokens(piece))
    return out


def _slug_tokens(s: str) -> set:
    """Raw subject-slug tokens without the norm length cut: split parts plus
    the joined form (`stc-108` → {stc, 108, stc108}), so digits and short
    parts that norm_tokens drops still participate in alias matching."""
    parts = [p for p in _SPLIT_RE.split(s.lower()) if p]
    return set(parts) | ({"".join(parts)} if len(parts) > 1 else set())


def _doc_tokens(s: str) -> list:
    """Subject-field token multiset: normalized tokens plus raw slug tokens."""
    return _norm_list(s) + sorted(_slug_tokens(s))


def _build_index(all_claims):
    """Per-field token multisets, document frequencies and average lengths.
    Every claim is indexed — the as-of/terminal filters are applied after
    ranking — and malformed claims contribute empty field docs."""
    docs = []
    df = {f: Counter() for f in _FIELD_WEIGHTS}
    dl_sum = {f: 0 for f in _FIELD_WEIGHTS}
    for c in all_claims:
        fields = {
            "subject": _doc_tokens(_text(c.get("subject"))),
            "predicate": _norm_list(_text(c.get("predicate"))),
            "value": _norm_list(_text(c.get("value", ""))),
        }
        for f, toks in fields.items():
            dl_sum[f] += len(toks)
            for t in set(toks):
                df[f][t] += 1
        docs.append(fields)
    n = len(all_claims)
    avgdl = {f: (dl_sum[f] / n if n else 0.0) for f in _FIELD_WEIGHTS}
    return docs, df, avgdl


def _idf(df_t: int, n_docs: int) -> float:
    """Robertson IDF: positive even for terms seen in every document."""
    return math.log(1.0 + (n_docs - df_t + 0.5) / (df_t + 0.5))


def _field_score(q_terms, idf, counts: Counter, dl: int, avgdl: float) -> float:
    """Weighted-within-field BM25 sum over the query terms present in one
    field document. `idf` is the precomputed per-field idf table."""
    k = _K1 * (1.0 - _B + _B * dl / avgdl) if avgdl else _K1
    total = 0.0
    for t in q_terms:
        tf = counts.get(t)
        if tf:
            total += idf[t] * (tf * (_K1 + 1.0)) / (tf + k)
    return total


def _page_files(wiki):
    """Sorted wiki/<kind>/<slug>.md paths for the title-alias index."""
    out = []
    for kind in synthesize.PAGE_KINDS:
        for name in pages._section_files(wiki, kind):
            out.append("wiki/%s/%s" % (kind, name))
    return sorted(out)


def _query_alias_keys(question: str) -> set:
    """Alias lookup keys from the raw question: every lowercase split part,
    each part's normalized form, and joined adjacent pairs — so "stc-108",
    "stc108" and "stc 108" all reach the `stc-108` subject alias."""
    parts = [p for p in _SPLIT_RE.split(question.lower()) if p]
    keys = set(parts)
    for i, p in enumerate(parts):
        keys |= norm_tokens(p)
        if i + 1 < len(parts):
            keys.add(parts[i] + parts[i + 1])
    # stopwords must never act as alias/anchor keys — a value sentence
    # starting with "What"/"The" would otherwise uniquely anchor that
    # subject into every same-pattern question (observed: `ptt` injected
    # by 'what' on every What-question)
    return keys - STOPWORDS


def _expand_subject_terms(wiki, question, q_tokens, claims_by_id, all_claims,
                          docs):
    """F1 subject-alias expansion: tokens added to the subject-field query
    only (predicate/value never see them — prevents bleed). Built per query,
    never persisted. Three deterministic sources:

    1. slug aliases — `stc-108` indexes as {stc, 108, stc108}; a query key
       hitting exactly one subject injects the matched key (a surface form
       the subject doc already contains), and generic keys like `stc`
       shared by many subjects expand nothing.
    2. content anchors — a query key occurring in exactly one subject's
       predicate/value docs (e.g. `blank` only ever appears under stc-108)
       injects that subject's canonical token set: the paraphrase bridge.
    3. page titles — a `wiki/<kind>/<slug>.md` frontmatter `title:` whose
       tokens overlap the question injects its dep claims' subject tokens,
       but only when the overlapping token isn't an ambiguous anchor (a
       bare `stc` shared by many subjects can't pick a page). Malformed
       pages are skipped with a warning."""
    warnings = []
    alias = {}
    anchor = {}
    for i, c in enumerate(all_claims):
        subj = _text(c.get("subject"))
        if not subj:
            continue
        for key in _slug_tokens(subj) | norm_tokens(subj):
            alias.setdefault(key, set()).add(subj)
        for t in set(docs[i]["predicate"]) | set(docs[i]["value"]):
            if t not in STOPWORDS:
                anchor.setdefault(t, set()).add(subj)

    q_keys = _query_alias_keys(question)
    expanded = set()
    for key in q_keys:
        hit = alias.get(key)
        if hit and len(hit) == 1:
            expanded.add(key)
            continue  # name match wins — suppress content anchor for this key
        ahit = anchor.get(key)
        if ahit and len(ahit) == 1:
            expanded |= set(_doc_tokens(next(iter(ahit))))

    for rel in _page_files(wiki):
        try:
            with open(wiki.p(rel), encoding="utf-8") as f:
                front = synthesize._split_front(f.read())[0]
        except Exception as e:
            warnings.append("alias index skipped %s: %s" % (rel, e))
            continue
        title = front.get("title")
        if not title:
            continue
        title_toks = norm_tokens(str(title)) - STOPWORDS
        hit_tokens = title_toks & q_keys
        # require the WHOLE title phrase to be echoed by the question —
        # one shared generic token ("plan") must not bridge an unrelated
        # page's deps into the query (observed: retired-rollout-plan
        # injecting order.event.delivery into the 2028-pricing question)
        if not title_toks or title_toks - hit_tokens:
            continue
        if not any(len(anchor.get(t, ())) <= 1
                   for t in hit_tokens):
            continue
        dep_list = front.get("deps")
        if not isinstance(dep_list, list):
            continue
        for dep in dep_list:
            did = str(dep).split("@", 1)[0]
            dep_claim = claims_by_id.get(did)
            if dep_claim:
                expanded |= set(_doc_tokens(_text(dep_claim.get("subject"))))
    return expanded - q_tokens, warnings



def prepare(wiki, question: str, as_of: Optional[str] = None) -> dict:
    q_tokens = norm_tokens(question) - STOPWORDS
    all_claims = claims.list_claims(wiki)
    subject_of = {c.get("id"): c.get("subject") for c in all_claims}
    claims_by_id = {c.get("id"): c for c in all_claims}

    docs, df, avgdl = _build_index(all_claims)
    n_docs = len(all_claims)
    idf = {f: {t: _idf(n, n_docs) for t, n in df[f].items()}
           for f in _FIELD_WEIGHTS}

    expanded, warnings = _expand_subject_terms(
        wiki, question, q_tokens, claims_by_id, all_claims, docs)
    q_subject = sorted(q_tokens | expanded)
    q_other = sorted(q_tokens)

    matches = []
    conflicts = []
    excluded = []
    for idx, c in enumerate(all_claims):
        parts = {}
        for f in _FIELD_WEIGHTS:
            counts = Counter(docs[idx][f])
            parts[f] = _FIELD_WEIGHTS[f] * _field_score(
                q_subject if f == "subject" else q_other,
                idf[f], counts, len(docs[idx][f]), avgdl[f])
        score = parts["subject"] + parts["predicate"] + parts["value"]
        parts["total"] = score
        if score <= 0:
            continue

        if as_of:
            # historical: claim's validity window must cover that date.
            # valid dates are ISO "YYYY-MM-DD"; as_of may be a date or a
            # datetime, so plain string comparison is the convention here.
            if c.get("valid_from") and c["valid_from"] > as_of:
                continue
            if c.get("valid_to") and c["valid_to"] < as_of:
                continue
            # Terminal claims count only when they were still live on that
            # date. The exact supersession/rejection moment is not a schema
            # field, so approximate it with updated_at (set on every claim
            # mutation), falling back to recorded_at for claims written by
            # older versions. When both are missing, keep the claim — the
            # validity window is the best data available.
            if c.get("status") in claims.TERMINAL:
                ended = c.get("updated_at") or c.get("recorded_at")
                if ended and ended[:10] <= as_of[:10]:
                    continue
        else:
            # current: only still-valid knowledge; token-matched terminal
            # claims surface separately as `superseded` (stale-knowledge gap)
            if c.get("status") in ("superseded", "rejected"):
                excluded.append({
                    "claim_id": c.get("id"),
                    "subject": c.get("subject"),
                    "predicate": c.get("predicate"),
                    "value": c.get("value"),
                    "status": c.get("status"),
                    "replaced_by": c.get("superseded_by"),
                })
                continue
            # valid_to is an inclusive end date: only a date strictly before
            # today ends validity — a future valid_to is still valid
            today = time.strftime("%Y-%m-%d", time.gmtime())
            if c.get("valid_to") and c["valid_to"] < today:
                continue

        entry = {
            "claim_id": c.get("id"),
            "subject": c.get("subject"),
            "predicate": c.get("predicate"),
            "value": c.get("value"),
            "unit": c.get("unit"),
            "scope": c.get("scope") or {},
            "status": c.get("status"),
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

        if c.get("status") == "disputed":
            conflicts.append({
                "claim_id": c.get("id"),
                "reason": "disputed claim matches the question",
            })

    matches.sort(key=lambda m: (-m["score"], _text(m["claim_id"])))
    # precision floor: drop tail matches below 40% of the top score — the
    # first match always survives (it is the max), so this only trims
    # noise, never empties a result that found anything.
    if matches:
        floor = matches[0]["score"] * 0.4
        matches = [m for m in matches if m["score"] >= floor]
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
        known |= norm_tokens("%s %s %s" % (_text(c.get("subject")),
                                           _text(c.get("predicate")),
                                           _text(c.get("value", ""))))
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
        "warnings": warnings,
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
