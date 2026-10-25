"""Context packs: the smallest useful context for a specific task, under a
hard token budget (PRD §19). Everything here is derived from committed state;
a pack is itself a registered artifact so staleness propagates (PRD §17).
"""
import json
import math
import os
import re
import time
from typing import Optional

from . import claims, deps, ids, synthesize
from .text import norm_tokens
from .transaction import Transaction, TxnError
from .yamlite import dumps as yamldumps

STOPWORDS = set("""a an and are as at be but by for from has have how i in is it its of on or
that the this to was we what when where which who why will with you our us do does did""".split())

_PAGE_LIMIT = 10
_SOURCE_LIMIT = 10
# Pack frontmatter deps and receipt sources are bounded to keep receipts
# readable, but the bound is deliberately high; any truncation is recorded
# explicitly in the receipt instead of dropped silently.
_DEPS_LIMIT = 500
_RECEIPT_SOURCES_LIMIT = 500

# Loose ISO-8601 "YYYY-MM-DD[ T]HH:MM[:SS]" recognizer for --changes-since;
# the trailing "Z"/offset is tolerated and dropped (wiki timestamps are UTC).
_TS_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?$")


def _norm_since_ts(value: str) -> Optional[str]:
    """Normalize an ISO-8601 timestamp for lexicographic comparison with
    operations' finished_at ("%Y-%m-%dT%H:%M:%SZ"). Returns None when the
    value is a context id or anything unrecognized."""
    v = (value or "").strip()
    if not _TS_RE.match(v):
        return None
    date = v[:10]
    if len(v) == 10:
        return date  # date-only: ops on that date compare after it
    time_part = v[11:19]
    if len(time_part) < 8:
        time_part = (time_part + ":00:00"[:8 - len(time_part)])
    return "%sT%sZ" % (date, time_part)


def _est(text: str) -> int:
    return max(1, math.ceil(len(text) / 4))


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _fmt_claim(c: dict) -> str:
    scope = ", ".join("%s=%s" % kv for kv in sorted((c.get("scope") or {}).items()))
    parts = ["- %s / %s = %s" % (c["subject"], c["predicate"], c.get("value"))]
    if c.get("unit"):
        parts.append(" %s" % c["unit"])
    if scope:
        parts.append(" [scope: %s]" % scope)
    if c.get("valid_from"):
        parts.append(" (valid from %s" % c["valid_from"])
        parts.append(", to %s)" % c["valid_to"] if c.get("valid_to") else ")")
    ev = c["evidence"][0] if c.get("evidence") else None
    if ev:
        parts.append(" — evidence: %s@%s %s" % (
            ev["source_id"][:18], ev["source_version"],
            ev.get("locator", {}).get("value", "") or ev.get("locator", {}).get("type", "")))
    return "".join(parts)

def _sections(wiki, task: str):
    """Candidate sections in PRD §19 priority order: (title, lines, claim_ids,
    other_deps). Sections whose every item is off-topic for a narrow task are
    dropped and reported as (title, reason) in the returned omitted list (Q5)."""
    cfg = wiki.load_config()
    policy = cfg["acceptance_policy"]
    ttask = norm_tokens(task) - STOPWORDS
    narrow = bool(ttask)

    def hits(text: str) -> int:
        # split snake_case too: predicate click_lookback_window matches "click window"
        return len(ttask & norm_tokens(text.replace("_", " "))) if narrow else 0

    def filtered(title: str, items: list, key) -> list:
        if not narrow:
            return items
        kept = [x for x in items if hits(key(x)) > 0]
        if items and not kept:
            omitted.append((title, "off-topic"))
        return kept

    def ctext(c):
        return "%s %s %s" % (c["subject"], c["predicate"], c.get("value"))

    omitted = []
    all_claims = claims.list_claims(wiki)
    # auto-materialized stubs are claim-derived placeholders, not authored
    # decisions — the claim itself already surfaces under Relevant claims
    decisions = [d for d in claims.list_decisions(wiki)
                 if not d.get("auto_materialized")]
    review = wiki.read_jsonl(".state/review-queue.jsonl")
    open_items = [r for r in review if r.get("status") == "open"]

    sections = []

    constraints = [c for c in all_claims
                   if c["status"] == "accepted"
                   and (c.get("authority") or {}).get("type") in policy["auto_accept_authority"]]
    constraints = filtered("Critical constraints", constraints, ctext)
    if constraints:
        sections.append(("Critical constraints",
                         [_fmt_claim(c) for c in constraints],
                         [c["id"] for c in constraints], []))

    decisions = filtered("Accepted decisions", decisions,
                         lambda d: "%s %s" % (d["title"], d.get("body") or ""))
    if decisions:
        lines = []
        for d in decisions:
            first = (d.get("body") or "").strip().splitlines()[0] if d.get("body") else d["title"]
            lines.append("- %s: %s" % (d["title"], first))
        sections.append(("Accepted decisions", lines, [], [d["id"] for d in decisions]))

    relevant = [c for c in all_claims
                if c["status"] in ("accepted", "provisional", "disputed", "candidate")]
    relevant.sort(key=lambda c: (hits(ctext(c)), c["status"] == "accepted"), reverse=True)
    relevant = [c for c in relevant if hits(ctext(c)) > 0 or not narrow][:15]
    if relevant:
        sections.append(("Relevant claims", [_fmt_claim(c) for c in relevant],
                         [c["id"] for c in relevant], []))

    impl = [c for c in all_claims
            if c["evidence"] and c["evidence"][0].get("locator", {}).get("type") == "repo_path"]
    impl = filtered("Implementation references", impl, ctext)
    if impl:
        sections.append(("Implementation references",
                         [_fmt_claim(c) for c in impl], [c["id"] for c in impl], []))

    open_items = filtered("Unresolved conflicts", open_items,
                          lambda r: r.get("problem", ""))
    if open_items:
        lines = ["- [%s] %s (affected: %s)" % (r["kind"], r["problem"],
                                               ", ".join(r.get("affected", [])))
                 for r in open_items]
        sections.append(("Unresolved conflicts", lines, [], [r["id"] for r in open_items]))

    # Related concepts: relevance-scored on title + body + subject tokens,
    # not filename only (Q5).
    scored = []
    cdir = wiki.p("wiki", "concepts")
    if os.path.isdir(cdir):
        for name in sorted(os.listdir(cdir)):
            if not name.endswith(".md"):
                continue
            rel = "wiki/concepts/%s" % name
            try:
                with open(wiki.p(rel)) as f:
                    front, body = synthesize._split_front(f.read())
            except OSError:
                continue
            score = hits("%s %s %s" % (front.get("title") or name[:-3],
                                       front.get("subject") or "", body))
            if narrow and score == 0:
                continue
            scored.append((score, rel))
    scored.sort(key=lambda t: (-t[0], t[1]))
    concept_pages = [rel for _s, rel in scored[:_PAGE_LIMIT]]
    if concept_pages:
        sections.append(("Related concepts", ["- %s" % p for p in concept_pages],
                         [], concept_pages))

    srcs_dir = wiki.p("sources")
    if os.path.isdir(srcs_dir):
        manifests = []
        for sid in sorted(os.listdir(srcs_dir))[:_SOURCE_LIMIT]:
            mpath = wiki.p("sources", sid, "manifest.json")
            if os.path.isfile(mpath):
                manifests.append(wiki.load_json("sources/%s/manifest.json" % sid))
        manifests = filtered("Supplementary evidence", manifests,
                             lambda m: m.get("title", ""))
        names = ["- %s (%s, %d version(s))" % (m.get("title", m["id"]), m["kind"],
                                               len(m["versions"])) for m in manifests]
        if names:
            sections.append(("Supplementary evidence", names, [], []))
    return sections, omitted

def _claim_version_map(wiki) -> dict:
    out = {c["id"]: c["version"] for c in claims.list_claims(wiki)}
    out.update({d["id"]: d["version"] for d in claims.list_decisions(wiki)})
    return out


def build(wiki, task: str, budget: Optional[int] = None, resume: bool = False,
          changes_since: Optional[str] = None) -> dict:
    if not wiki.exists():
        raise TxnError("wiki not initialized")
    cfg = wiki.load_config()
    if budget is None:
        budget = int(cfg["context_budget_default"])
    else:
        budget = int(budget)
    if budget <= 0:
        # 0 is an explicit (invalid) budget, not "unset": refuse rather than
        # silently fall back to the configured default
        raise TxnError("budget must be a positive token count (got %d)" % budget)
    context_id = ids.new("context")
    versions = _claim_version_map(wiki)

    task_label = task or ("resume briefing" if resume else "change briefing")

    if resume or changes_since:
        ref, briefing_claims = _briefing(wiki, changes_since)
        blocks = [("Change briefing", briefing_claims["lines"],
                   briefing_claims["claim_ids"], [])]
        omitted = []
    else:
        ref = None
        blocks, omitted = _sections(wiki, task)

    header = ["# Context pack %s" % context_id, "",
              "- Task: %s" % task_label,
              "- Generated: %s" % _now(),
              "- Budget: %d (estimated tokens, hard)" % budget,
              "- Evidence reference: %s" % ((ref or {}).get("context_id", "current state")),
              ""]

    # Greedy inclusion in priority order, then drop-from-end until the FULL
    # rendered file fits the budget (two-pass hard gate). Sections dropped for
    # budget — like sections filtered as off-topic — are recorded as omitted
    # markers so the agent knows what it did not see (Q5).
    included = list(blocks)
    while True:
        body = list(header)
        claim_ids, other_deps = [], []
        for title, lines, cids, others in included:
            body += ["## %s" % title, ""] + lines + [""]
            claim_ids += cids
            other_deps += others
        if omitted:
            body += ["## Omitted sections", ""]
            body += ["- %s (%s)" % (t, reason) for t, reason in omitted] + [""]
        seen = set()
        claim_ids = [c for c in claim_ids if not (c in seen or seen.add(c))]
        deps_list = ["%s@%s" % (c, versions.get(c, 1)) for c in claim_ids]
        deps_list += [d if "@" in d else "%s@%s" % (d, versions.get(d, 1))
                      for d in other_deps]
        seen2 = set()
        deps_list = [d for d in deps_list if not (d in seen2 or seen2.add(d))]
        deps_truncated = max(0, len(deps_list) - _DEPS_LIMIT)
        front = yamldumps({"type": "context-pack", "context": context_id,
                           "deps": deps_list[:_DEPS_LIMIT]})
        pack_text = "---\n%s\n---\n\n%s" % (front, "\n".join(body) + "\n")
        if _est(pack_text) <= budget or not included:
            break
        dropped = included.pop()  # lowest-priority section dropped; try again
        omitted.append((dropped[0], "budget"))

    est = _est(pack_text)
    if est > budget:
        raise TxnError("cannot fit context pack in budget %d "
                       "(minimum required is %d estimated tokens)"
                       % (budget, est))

    txn = Transaction(wiki, "wiki-context")
    pack_rel = "context/%s.md" % context_id
    txn.stage_write(pack_rel, pack_text)

    src_ids = sorted({ev["source_id"] for c in claims.list_claims(wiki)
                      if c["id"] in claim_ids for ev in c.get("evidence", [])})
    receipt = {
        "context_id": context_id,
        "task": task_label,
        "generated_at": _now(),
        "budget": {"requested": budget, "estimated": est},
        "claims": ["%s@%s" % (c, versions.get(c, 1)) for c in claim_ids],
        "decisions": ["%s@%s" % (d, versions.get(d, 1))
                      for d in other_deps if d.startswith("decision_")],
        "sources": src_ids[:_RECEIPT_SOURCES_LIMIT],
        "sources_truncated": max(0, len(src_ids) - _RECEIPT_SOURCES_LIMIT),
        "deps_truncated": deps_truncated,
        "omitted": ["%s (%s)" % (t, reason) for t, reason in omitted],
    }
    txn.stage_write("context/%s.receipt.json" % context_id, receipt)
    deps.register(txn, pack_rel, deps_list)
    txn.changes = {"context_packs": 1}
    txn.commit(wiki.revision())
    return {
        "context_id": context_id,
        "pack_path": pack_rel,
        "receipt_path": "context/%s.receipt.json" % context_id,
        "receipt": receipt,
    }

def _ops_after(wiki, since_ts: str) -> list:
    # second-granularity timestamps: include ops committed in the same second
    return [op for op in wiki.read_jsonl(".state/operations.jsonl")
            if op.get("finished_at", "") >= since_ts and since_ts]

def _last_receipt(wiki, before_id: Optional[str]) -> dict:
    """Reference point for a change briefing. With a context id: the receipt
    recorded for exactly that pack. With an ISO-8601 timestamp: a synthetic
    reference at that time. Unset: the most recent receipt, falling back to
    project start."""
    ctx_dir = wiki.p("context")
    receipts = []
    if os.path.isdir(ctx_dir):
        for name in sorted(os.listdir(ctx_dir)):
            if not name.endswith(".receipt.json"):
                continue
            try:
                with open(os.path.join(ctx_dir, name)) as f:
                    r = json.load(f)
            except (OSError, ValueError):
                continue
            if r.get("context_id") and r.get("generated_at"):
                receipts.append(r)
    if before_id:
        # the requested pack's own receipt is the intended diff base
        match = next((r for r in receipts if r["context_id"] == before_id), None)
        if match:
            return match
        ts = _norm_since_ts(before_id)
        if ts is not None:
            return {"generated_at": ts, "context_id": before_id}
        # unrecognized reference: diff everything rather than silently guess
        return {"generated_at": "", "context_id": before_id}
    if receipts:
        return max(receipts, key=lambda r: (r["generated_at"], r["context_id"]))
    return {"generated_at": "", "context_id": "project-start"}

def _briefing(wiki, changes_since: Optional[str]) -> tuple:
    ref = _last_receipt(wiki, changes_since)
    ops = _ops_after(wiki, ref["generated_at"])
    lines = []
    if not ops:
        lines.append("No committed wiki operations since reference %s."
                     % ref.get("context_id", "project start"))
        return ref, {"lines": lines, "claim_ids": []}

    all_claims = claims.list_claims(wiki)
    # commit-time (wall clock), not semantic recorded_at: fixtures pin recorded_at
    # to content dates, so use updated_at / acceptance.accepted_at as commit signal
    def commit_ts(c):
        return (c.get("updated_at") or (c.get("acceptance") or {}).get("accepted_at")
                or c["recorded_at"])
    changed = [c for c in all_claims if commit_ts(c) >= ref["generated_at"]]
    superseded = [c for c in all_claims if c["status"] == "superseded"
                  and commit_ts(c) >= ref["generated_at"]]
    current_by_subject = {}
    for c in all_claims:
        if c["status"] in ("accepted", "provisional"):
            current_by_subject.setdefault((c["subject"], c["predicate"]), c)

    lines.append("Operations since reference %s: %d" % (ref.get("context_id"), len(ops)))
    if changed:
        lines += ["", "### New or changed claims"]
        lines += [_fmt_claim(c) for c in changed]
    if superseded:
        lines += ["", "### Recently superseded assumptions"]
        for c in superseded:
            repl = _replacement_of(all_claims, c)
            lines.append("- %s (%s / %s, was %s → now %s)" % (
                c["id"], c["subject"], c["predicate"], c.get("value"),
                repl.get("value") if repl else "?"))
    open_items = [r for r in wiki.read_jsonl(".state/review-queue.jsonl")
                  if r.get("status") == "open"]
    if open_items:
        lines += ["", "### Open questions / known problems"]
        lines += ["- [%s] %s" % (r["kind"], r["problem"]) for r in open_items]
    lines += ["", "### Snapshot",
              "- claims: %d, decisions: %d" % (len(all_claims), len(claims.list_decisions(wiki)))]
    return ref, {"lines": lines, "claim_ids": [c["id"] for c in changed]}

def _replacement_of(all_claims: list, c: dict) -> Optional[dict]:
    """Resolve the claim that replaced a superseded one. Prefer the explicit
    superseded_by pointer; fall back to scanning supersedes lists for wikis
    written before that field was populated. Self-references are ignored."""
    by_id = {s["id"]: s for s in all_claims}
    rid = c.get("superseded_by")
    if rid and rid != c["id"] and rid in by_id:
        return by_id[rid]
    return next((s for s in all_claims
                 if s["id"] != c["id"] and c["id"] in (s.get("supersedes") or [])),
                None)
