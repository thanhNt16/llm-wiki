#!/usr/bin/env python3
"""Deterministic survival/trap corpus generator (crow-hq long2+dup patterns).

Writes corpus/ (docs + per-doc candidate batches + labels.json) with planted
facts at controlled positions, corrections, corroboration twins, scope twins,
shared-value and near-subject traps. Pure stdlib, no randomness: rerunning
reproduces byte-identical output.

    python3 evals/survival/make_corpus.py [--out corpus]
"""
import argparse
import json
import os

FILLER_SECTIONS = [
    ("Platform overview",
     "The platform runs three regions with an active-active control plane. "
     "Capacity planning follows the quarterly review cycle; each review "
     "produces a rolling forecast that the owning teams reconcile against "
     "actuals. {}"),
    ("Traffic patterns",
     "Weekday traffic peaks between 18:00 and 21:00 UTC. The routing layer "
     "shifts read traffic to the standby region when the primary exceeds its "
     "soft budget. {}"),
    ("Data platform",
     "The warehouse ingests via two paths: a batch loader for historical "
     "backfills and a streaming path for near-real-time events. Schema "
     "evolution is governed by the contract registry. {}"),
    ("Reliability",
     "Error budgets are tracked per service. A service that exhausts its "
     "budget freezes feature work until the burn rate returns under "
     "control. {}"),
    ("Security posture",
     "Quarterly access reviews cover every production role. Break-glass "
     "access is alarmed and auto-expires after four hours. {}"),
    ("Cost outlook",
     "Unit cost per order trended down through the quarter. The largest "
     "contributors to savings were storage tiering and the search shard "
     "consolidation. {}"),
    ("Support load",
     "Ticket volume concentrated on checkout failures early in the quarter "
     "and fell after the timeout raise shipped. {}"),
    ("Vendor notes",
     "The payment gateway contract renews in Q4; the renegotiation aims to "
     "keep the retry behaviour as implemented today. {}"),
    ("Hiring",
     "Two platform engineers and one data engineer joined; onboarding "
     "material now includes the runbook reading list. {}"),
    ("Roadmap",
     "Next quarter focuses on the event retention rollout, the ledger "
     "export pipeline, and retiring the legacy analytics job. {}"),
]

FILLER_BODY = (
    "Teams reviewed the operating metrics weekly. The narrative in this "
    "section is filler prose: it exists so that planted facts land at "
    "controlled line positions in a long document, exercising ingest, "
    "hashing, staging and locator resolution far past the first screen. "
    "None of these sentences state a claim that the labels expect. "
    "The paragraphs repeat with small variations to reach the target "
    "length without introducing noise facts."
)

def _candidate(sid, subject, predicate, value, authority, scope=None,
               valid_from=None, locator=None):
    ev = {"source_id": sid, "source_version": 1,
          "locator": locator or {"type": "heading", "value": "Body"}}
    return {
        "id": "auto", "subject": subject, "predicate": predicate,
        "value": value, "scope": scope or {}, "valid_from": valid_from,
        "valid_to": None, "recorded_at": "2026-11-15T00:00:00Z",
        "status": "candidate", "proposed_by": "survival",
        "authority": {"type": authority},
        "evidence": [ev], "supersedes": [],
    }

def build_long_doc():
    """~800-line report with planted facts at early/mid/late/deep positions.

    Returns (text, facts) where facts carry the planted line positions for
    labels (line numbers are measured after assembly).
    """
    lines = ["# Platform report Q3 2026", "",
             "## TL;DR", "",
             "Quarter in review. Attribution window is 30 days; checkout "
             "stabilised after the timeout raise; the search consolidation "
             "finished ahead of schedule.", "",
             "> Note: figures in the TL;DR are the Q2 carry-over snapshot; "
             "the authoritative Q3 numbers are in the body sections.", ""]
    target_lines = 800
    facts = {}

    def plant(key, section_idx, rel_pos, sentence):
        """Record sentence to inject at rel_pos within filler section."""
        facts[key] = {"section": section_idx, "rel": rel_pos,
                      "sentence": sentence}

    # planted facts: key -> (subject, predicate, value, authority)
    plant("f1-early", 1, 0.5, "The checkout hard timeout is 3000 ms.")
    plant("f2-mid", 4, 0.5, "Search runs on 24 shards per region.")
    plant("f3-late", 8, 0.5, "Warehouse vacuum runs 02:00-04:00 UTC.")
    facts["f4-deep"] = {"section": 99, "rel": 0.98,
                        "sentence": "Ledger exports are written as parquet."}

    target = 800
    n_sec = len(FILLER_SECTIONS)
    quota = max(6, (target - len(lines) - 6) // n_sec)  # lines per block
    body = list(lines)
    for i, (title, tmpl) in enumerate(FILLER_SECTIONS):
        block = ["## %s" % title, "", tmpl.format(FILLER_BODY)]
        sec_facts = {k: f for k, f in facts.items() if f["section"] == i}
        while len(block) < quota - 2:
            block.append(FILLER_BODY)
        for key, f in sec_facts.items():
            at = min(max(3, int(quota * f["rel"])), len(block))
            block.insert(at, f["sentence"])
        body.extend(block)
        body.append("")
    # deep fact gets its own nested appendix near EOF
    body += ["### Appendix: ledger export detail", "",
             facts["f4-deep"]["sentence"], "", "End of report.", ""]
    text = "\n".join(body) + "\n"

    # measure actual line positions of planted sentences
    doc_lines = text.split("\n")
    for key, f in facts.items():
        f["line"] = next(i for i, l in enumerate(doc_lines, 1)
                         if l.strip() == f["sentence"].strip())
        f["pct"] = f["line"] / len(doc_lines)
    return text, facts

LONG_FACT_META = {
    "f1-early": ("checkout", "hard-timeout-ms", 3000, "authoritative_source"),
    "f2-mid": ("search", "shard-count", 24, "authoritative_source"),
    "f3-late": ("warehouse", "vacuum-window", "02:00-04:00 UTC",
                "authoritative_source"),
    "f4-deep": ("ledger", "export-format", "parquet",
                "authoritative_source"),
}

def build(out):
    os.makedirs(out, exist_ok=True)
    docs, batches, labels = {}, [], []

    # --- batch 1: the long report (all auto-UNRELATED; no existing claims)
    text, pos = build_long_doc()
    docs["long/platform-report-q3.md"] = text
    cands = []
    facts_meta = dict(LONG_FACT_META)
    facts_meta["f5a-tldr"] = ("attribution", "window-days", 30,
                              "authoritative_source")
    for key, (subj, pred, val, auth) in facts_meta.items():
        loc = {"type": "line_range", "line_start": 1, "line_end": 1}
        if key in pos:
            loc = {"type": "line_range",
                   "line_start": pos[key]["line"],
                   "line_end": pos[key]["line"]}
        cands.append(_candidate("SRC", subj, pred, val, auth,
                                valid_from="2026-07-01", locator=loc))
        bucket = None
        if key in pos:
            pct = pos[key]["pct"]
            bucket = ("early" if pct < 0.25 else
                      "mid" if pct < 0.75 else "late" if pct < 0.95
                      else "deep")
        labels.append({
            "id": key, "kind": "planted", "bucket": bucket,
            "subject": subj, "predicate": pred, "scope": {},
            "value": val, "status": "accepted",
            "evidence_count": 1, "superseded_by": None,
            "locator_line": pos[key]["line"] if key in pos else None,
        })
    batches.append({"doc": "long/platform-report-q3.md", "cands": cands,
                    "classifications": []})  # exercise the auto path

    def add_batch(doc_rel, cands, classifications, label_defs):
        docs[doc_rel] = DOCS_TEXT[doc_rel]
        batches.append({"doc": doc_rel, "cands": cands,
                        "classifications": classifications})
        labels.extend(label_defs)

    # --- corroboration twins: same claim from two independent docs
    DOCS_TEXT = {}
    DOCS_TEXT["traps/twin-a.md"] = (
        "# Pipeline rollout note\n\nThe attribution pipeline moved to a "
        "staged rollout this quarter.\n")
    DOCS_TEXT["traps/twin-b.md"] = (
        "# Ops weekly\n\nReminder from ops sync: attribution pipeline is on "
        "a staged rollout, canary first.\n")
    add_batch("traps/twin-a.md",
              [_candidate("SRC", "attribution pipeline", "rollout-mode",
                          "canary", "independent_corroboration")],
              [],
              [{"id": "corr-twin", "kind": "corroboration",
                "subject": "attribution pipeline", "predicate":
                "rollout-mode", "scope": {}, "value": "canary",
                "status": "accepted", "evidence_count": 2,
                "superseded_by": None}])
    add_batch("traps/twin-b.md",
              [_candidate("SRC", "attribution pipeline", "rollout-mode",
                          "canary", "independent_corroboration")],
              [],  # auto: CORROBORATION (different root origin)
              [])

    # --- shared-value trap: 7-day retention window vs 7-day attribution
    DOCS_TEXT["traps/retention.md"] = (
        "# Retention settings\n\nThe event-retention retention-days setting "
        "is 90 and the general retention window is 7 days.\n")
    add_batch("traps/retention.md",
              [_candidate("SRC", "retention", "window-days", 7,
                          "authoritative_source"),
               _candidate("SRC", "event-retention", "retention-days", 90,
                          "authoritative_source")],
              [],
              [{"id": "shared-value", "kind": "trap-shared-value",
                "subject": "retention", "predicate": "window-days",
                "scope": {}, "value": 7, "status": "accepted",
                "evidence_count": 1, "superseded_by": None},
               {"id": "er-90", "kind": "superseded-old",
                "subject": "event-retention", "predicate": "retention-days",
                "scope": {}, "value": 90, "status": "superseded",
                "evidence_count": 1, "superseded_by": "er-45",
                "bucket": None}])

    # --- near-subject trap: "Event Retention" folds to event-retention at t2
    DOCS_TEXT["traps/near-subject.md"] = (
        "# Draft note\n\nEvent Retention retention-days is 90 per the old "
        "draft (subject spelling differs on purpose).\n")
    add_batch("traps/near-subject.md",
              [_candidate("SRC", "Event Retention", "retention-days", 90,
                          "agent_inference")],
              [],  # auto: rejected_tier_match -> review item, no claim
              [{"id": "near-subject", "kind": "trap-near-subject",
                "subject": "event-retention", "predicate": "retention-days",
                "scope": {}, "value": 90, "status": "rejected_tier_match",
                "evidence_count": 0, "superseded_by": None,
                "review_item": True}])

    # --- scope twins: same value, different environment (S4 shape)
    DOCS_TEXT["traps/scope-prod.md"] = (
        "# Audit config (production)\n\nAudit log retention is 365 days in "
        "production.\n")
    DOCS_TEXT["traps/scope-sandbox.md"] = (
        "# Audit config (sandbox)\n\nAudit log retention is 365 days in "
        "sandbox too.\n")
    add_batch("traps/scope-prod.md",
              [_candidate("SRC", "audit-log", "retention-days", 365,
                          "authoritative_source",
                          scope={"environment": "production"})],
              [],
              [{"id": "scope-prod", "kind": "scope-twin",
                "subject": "audit-log", "predicate": "retention-days",
                "scope": {"environment": "production"}, "value": 365,
                "status": "accepted", "evidence_count": 1,
                "superseded_by": None}])
    add_batch("traps/scope-sandbox.md",
              [_candidate("SRC", "audit-log", "retention-days", 365,
                          "authoritative_source",
                          scope={"environment": "sandbox"})],
              [{"index": 0, "relationship": "SCOPE_DIFFERENCE",
                "target_claim_id": None, "valid_from": None,
                "valid_to": None,
                "authority": {"type": "authoritative_source"}}],
              [{"id": "scope-sandbox", "kind": "scope-twin",
                "subject": "audit-log", "predicate": "retention-days",
                "scope": {"environment": "sandbox"}, "value": 365,
                "status": "accepted", "evidence_count": 1,
                "superseded_by": None}])

    # --- same subject, different predicate: no conflict
    DOCS_TEXT["traps/diff-predicate.md"] = (
        "# Retention enforcement\n\nEvent-retention enforcement is lazy: "
        "rows expire on read.\n")
    add_batch("traps/diff-predicate.md",
              [_candidate("SRC", "event-retention", "enforcement", "lazy",
                          "authoritative_source")],
              [],
              [{"id": "er-enforcement", "kind": "trap-diff-predicate",
                "subject": "event-retention", "predicate": "enforcement",
                "scope": {}, "value": "lazy", "status": "accepted",
                "evidence_count": 1, "superseded_by": None}])

    # --- correction batch 1: long-doc addendum supersedes the TL;DR 30d
    DOCS_TEXT["long/platform-report-addendum.md"] = (
        "# Platform report Q3 addendum\n\nCorrection: the attribution "
        "window carried over from Q2 (30 days) is superseded; the Q3 "
        "attribution window is 7 days effective 2026-09-01.\n")
    add_batch("long/platform-report-addendum.md",
              [_candidate("SRC", "attribution", "window-days", 7,
                          "authoritative_source",
                          valid_from="2026-09-01")],
              [{"index": 0, "relationship": "CORRECTION",
                "target_claim_id": "RESOLVE:attribution/window-days/30",
                "valid_from": "2026-09-01", "valid_to": None,
                "authority": {"type": "authoritative_source"}}],
              [{"id": "f5b-correction", "kind": "correction-current",
                "subject": "attribution", "predicate": "window-days",
                "scope": {}, "value": 7, "status": "accepted",
                "evidence_count": 1, "superseded_by": None}])
    for lab in labels:
        if lab["id"] == "f5a-tldr":
            lab["status"] = "superseded"
            lab["superseded_by"] = "f5b-correction"
            lab["kind"] = "superseded-old"

    # --- correction batch 2: value change on a trap-set claim (90 -> 45)
    DOCS_TEXT["traps/retention-change.md"] = (
        "# Retention change\n\nEvent retention drops from 90 to 45 days "
        "effective 2026-10-01.\n")
    add_batch("traps/retention-change.md",
              [_candidate("SRC", "event-retention", "retention-days", 45,
                          "authoritative_source",
                          valid_from="2026-10-01")],
              [{"index": 0, "relationship": "CORRECTION",
                "target_claim_id": "RESOLVE:event-retention/retention-days/90",
                "valid_from": "2026-10-01", "valid_to": None,
                "authority": {"type": "authoritative_source"}}],
              [{"id": "er-45", "kind": "correction-current",
                "subject": "event-retention", "predicate": "retention-days",
                "scope": {}, "value": 45, "status": "accepted",
                "evidence_count": 1, "superseded_by": None}])

    # write docs + candidate files
    for rel, text in docs.items():
        p = os.path.join(out, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(text)
    os.makedirs(os.path.join(out, "candidates"), exist_ok=True)
    for i, b in enumerate(batches):
        name = os.path.basename(b["doc"]).replace(".md", "")
        with open(os.path.join(out, "candidates",
                               "%02d-%s.jsonl" % (i, name)), "w") as f:
            for c in b["cands"]:
                f.write(json.dumps(c, ensure_ascii=False) + "\n")
    ingest_order = [b["doc"] for b in batches]
    manifest = {"ingest_order": ingest_order,
                "labels": labels,
                "review_expect": [
                    {"kind": "rejected_tier_match",
                     "subject": "Event Retention",
                     "predicate": "retention-days"}]}
    with open(os.path.join(out, "labels.json"), "w") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False)
    print("wrote %d docs, %d batches, %d labels -> %s"
          % (len(docs), len(batches), len(labels), out))

if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "corpus"))
    a = ap.parse_args()
    build(a.out)
