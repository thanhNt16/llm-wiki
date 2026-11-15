#!/usr/bin/env python3
"""Deterministic planted-facts corpus generator for the rust-vs-python
quality lane (P7 lane A).

    python3 gen_corpus.py --out corpus [--docs N] [--facts-per-doc M] [--seed S]

Emits:
    <out>/docs/doc-NNN.md          planted-fact documents (v1 content)
    <out>/v2/doc-NNN.md            corrected content for correction cases
    <out>/candidates/doc-NNN.jsonl staged-candidate files (v1 facts)
    <out>/candidates_v2/doc-NNN.jsonl corrected candidates
    <out>/PROJECT.md               hand-maintained summary (baseline reading)
    <out>/labels.json              per-fact labels for scoring

Same inputs -> byte-identical output (fixed seed, stable iteration order,
no timestamps).

Stdlib only, Python >= 3.9.
"""
import argparse
import json
import os
import random

DEFAULT_SEED = 20261115

# predicate -> (phrase used in the fact sentence, value kind)
PREDICATES = [
    ("cache_ttl_seconds", "cache ttl seconds", "int_60_3600"),
    ("max_connections", "max connections", "int_1_512"),
    ("retry_count", "retry count", "int_1_9"),
    ("region", "region", "region"),
    ("deploy_strategy", "deploy strategy", "strategy"),
    ("owner", "owner", "name"),
    ("oncall_slack_channel", "oncall slack channel", "channel"),
    ("api_version", "api version", "version"),
    ("alert_threshold_pct", "alert threshold pct", "int_50_99"),
    ("backup_window_utc", "backup window utc", "clock"),
]

REGIONS = ["us-east-1", "eu-west-2", "ap-south-1", "us-west-2", "eu-central-1"]
STRATEGIES = ["rolling-restart", "blue-green", "canary", "recreate"]
FIRST = ["Alice", "Bruno", "Carmen", "Devon", "Elena", "Farid", "Greta",
         "Henrik", "Imani", "Jonas", "Keiko", "Lars", "Mira", "Nadia"]
LAST = ["Rivera", "Okafor", "Lindqvist", "Tanaka", "Novak", "Petrov",
        "Santos", "Weber", "Yilmaz", "Costa", "Berg", "Dubois"]

# Docs whose fact[0] is a correction case (v1 asserted, corrected by v2).
CORRECTION_DOCS = (5, 15, 25)
# (doc_a, doc_b, subject, predicate, phrase, value_a, value_b)
CONFLICT_CASES = (
    (8, 9, "svc.shared-cache", "eviction_policy", "eviction policy",
     "lru", "lfu"),
    (18, 19, "svc.shared-rate", "rate_limit_rpm", "rate limit rpm",
     "1200", "950"),
)
# Docs whose fact[3] is a baseline "trap": a platform.global fact placed in
# late-numbered docs so keyword ranking buries it behind earlier files.
TRAP_DOCS = (31, 32, 33, 34)
TRAP_PREDICATES = [
    ("owner", "owner", "name"),
    ("region", "region", "region"),
    ("deploy_strategy", "deploy strategy", "strategy"),
    ("alert_threshold_pct", "alert threshold pct", "int_50_99"),
]
TRAP_SUBJECT = "platform.global"

FILLER = [
    "This note captures day to day operational detail kept for reference.",
    "History: the service was migrated during the spring consolidation.",
    "Runbook entries below are reviewed at the quarterly audit.",
    "Dashboards and alerts live in the shared monitoring workspace.",
    "Capacity numbers here are planning figures, not guarantees.",
    "Incidents page the rotation; see the escalation section for paging.",
    "Deployments run from the release pipeline on weekday mornings.",
    "The notes section records context gathered from review meetings.",
    "Dependencies are pinned in the manifest and updated monthly.",
    "Access is mediated by the central identity provider.",
]


def _value(rng, kind, salt):
    """Deterministic value for (kind, salt). rng state advances uniformly."""
    if kind == "int_60_3600":
        return str(60 + rng.randrange(0, 3541))
    if kind == "int_1_512":
        return str(1 + rng.randrange(0, 512))
    if kind == "int_1_9":
        return str(1 + rng.randrange(0, 9))
    if kind == "int_50_99":
        return str(50 + rng.randrange(0, 50))
    if kind == "region":
        return REGIONS[(salt + rng.randrange(0, len(REGIONS))) % len(REGIONS)]
    if kind == "strategy":
        return STRATEGIES[(salt + rng.randrange(0, len(STRATEGIES)))
                          % len(STRATEGIES)]
    if kind == "name":
        return "%s %s" % (FIRST[(salt + rng.randrange(0, len(FIRST)))
                                % len(FIRST)],
                          LAST[(salt + rng.randrange(0, len(LAST)))
                               % len(LAST)])
    if kind == "channel":
        return "ops-channel-%d" % (10 + rng.randrange(0, 90))
    if kind == "version":
        return "v%d.%d.%d" % (1 + rng.randrange(0, 4), rng.randrange(0, 10),
                              rng.randrange(0, 20))
    if kind == "clock":
        return "%02d:%02d" % (rng.randrange(0, 24), rng.randrange(0, 12) * 5)
    raise ValueError("unknown value kind %r" % kind)


def _value_v2(rng, kind, salt, v1):
    """A corrected value guaranteed != v1."""
    for _ in range(64):
        v = _value(rng, kind, salt)
        if v != v1:
            return v
    raise RuntimeError("could not derive distinct v2 value")


def _fact_sentence(phrase, subject, value):
    return "The %s of %s is %s." % (phrase, subject, value)


def _question(phrase, subject):
    return "What is the %s of %s?" % (phrase, subject)


def _regex_for(value):
    return r"(?<![a-z0-9])%s(?![a-z0-9])" % value.replace("\\", "\\\\")


def _doc_body(rng, subject, facts, header_subject):
    """Doc markdown: header + filler with facts injected at varied positions.

    facts: list of (phrase, fact_subject, value) already ordered as they
    should appear among the filler lines.
    """
    lines = ["# Operations notes for %s" % header_subject, ""]
    if header_subject != TRAP_SUBJECT:
        lines += ["Maintained by the %s platform team." % TRAP_SUBJECT, ""]
    n_filler = 6 + rng.randrange(0, 4)
    fillers = [FILLER[rng.randrange(0, len(FILLER))] for _ in range(n_filler)]
    # Slots: 0 .. n_filler inclusive -> fact i inserted before filler slot i.
    slots = sorted(rng.randrange(0, n_filler + 1) for _ in facts)
    body = []
    fi = 0
    for slot in range(n_filler + 1):
        while fi < len(facts) and slots[fi] == slot:
            body.append(_fact_sentence(*facts[fi]))
            body.append("")
            fi += 1
        if slot < n_filler:
            body.append(fillers[slot])
            body.append("")
    lines += body
    return "\n".join(lines).rstrip() + "\n"


def generate(out_dir, n_docs, facts_per_doc, seed):
    if facts_per_doc < 4:
        raise SystemExit("--facts-per-doc must be >= 4 (correction/conflict/"
                         "trap slots consume 0,1,3)")
    if n_docs <= max(max(CORRECTION_DOCS), max(TRAP_DOCS),
                     max(d for c in CONFLICT_CASES for d in c[:2])):
        raise SystemExit("--docs must exceed %d for planted cases"
                         % max(max(CORRECTION_DOCS), max(TRAP_DOCS),
                               max(d for c in CONFLICT_CASES
                                   for d in c[:2])))
    rng = random.Random(seed)
    os.makedirs(os.path.join(out_dir, "docs"), exist_ok=True)
    os.makedirs(os.path.join(out_dir, "v2"), exist_ok=True)
    os.makedirs(os.path.join(out_dir, "candidates"), exist_ok=True)
    os.makedirs(os.path.join(out_dir, "candidates_v2"), exist_ok=True)

    conflict_by_doc = {}
    for doc_a, doc_b, subj, pred, phrase, va, vb in CONFLICT_CASES:
        conflict_by_doc[doc_a] = (subj, pred, phrase, va, doc_b, vb)
        conflict_by_doc[doc_b] = (subj, pred, phrase, vb, doc_a, va)

    labels = {"docs": n_docs, "facts_per_doc": facts_per_doc,
              "seed": seed, "facts": []}
    summary_rows = []
    pred_cycle = 0

    for i in range(n_docs):
        subject = "svc.alpha-%03d" % i
        doc_rel = "docs/doc-%03d.md" % i
        doc_facts = []          # (phrase, subject, value) in sentence order
        doc_cands = []
        v2_facts = []           # corrections: (idx, v1, v2)
        # pick M distinct predicates for this doc
        chosen = []
        while len(chosen) < facts_per_doc:
            p = PREDICATES[pred_cycle % len(PREDICATES)]
            pred_cycle += 1
            if p[0] not in [c[0] for c in chosen]:
                chosen.append(p)
        for fi in range(facts_per_doc):
            fact_subject, pred, phrase, kind = (
                subject, *chosen[fi])
            correction = i in CORRECTION_DOCS and fi == 0
            conflict_peer = None
            trap = i in TRAP_DOCS and fi == facts_per_doc - 1
            if conflict_peer is None and i in conflict_by_doc and fi == 1:
                (c_subj, c_pred, c_phrase, c_val, peer_doc, peer_val
                 ) = conflict_by_doc[i]
                fact_subject, pred, phrase = c_subj, c_pred, c_phrase
                value = c_val
                conflict_peer = {
                    "doc": "docs/doc-%03d.md" % peer_doc,
                    "value_exact": peer_val,
                    "value_regex": _regex_for(peer_val),
                }
            elif trap:
                pred, phrase, kind = TRAP_PREDICATES[TRAP_DOCS.index(i)]
                fact_subject = TRAP_SUBJECT
                value = _value(rng, kind, i * 97 + fi)
            else:
                value = _value(rng, chosen[fi][2] if fi < len(chosen)
                               else kind, i * 101 + fi)
            fact = {
                "id": "f-%03d-%d" % (i, fi),
                "doc_index": i,
                "subject": fact_subject,
                "predicate": pred,
                "question": _question(phrase, fact_subject),
                "expected_value": {"exact": value,
                                   "regex": _regex_for(value)},
                "source_file": doc_rel,
                "locator_hint": _fact_sentence(phrase, fact_subject,
                                               value)[:-1],
                "correction_case": correction,
                "conflict_case": conflict_peer is not None,
                "trap_case": trap,
                "in_summary": fi < 2 or correction,
            }
            if conflict_peer:
                fact["conflict_peer"] = conflict_peer
            if correction:
                v2 = _value_v2(rng, kind if trap else chosen[fi][2],
                               i * 103 + fi, value)
                fact["correction"] = {
                    "file_v2": "v2/doc-%03d.md" % i,
                    "expected_value_v2": {"exact": v2,
                                          "regex": _regex_for(v2)},
                    "locator_hint_v2": _fact_sentence(phrase, fact_subject,
                                                    v2)[:-1],
                }
                v2_facts.append((fi, value, v2, pred, fact_subject))
            labels["facts"].append(fact)
            doc_facts.append((phrase, fact_subject, value))
            doc_cands.append({
                "subject": fact_subject, "predicate": pred,
                "value": int(value) if value.isdigit() else value,
                "locator": "s:" + _fact_sentence(phrase, fact_subject,
                                                 value)[:-1],
                "authority": "doc"})
            if fact["in_summary"]:
                summary_rows.append((fact["id"], fact_subject, pred,
                                     value, doc_rel))

        v1_text = _doc_body(rng, subject, doc_facts, subject)
        with open(os.path.join(out_dir, doc_rel), "w") as f:
            f.write(v1_text)
        with open(os.path.join(out_dir, "candidates",
                               "doc-%03d.jsonl" % i), "w") as f:
            for c in doc_cands:
                f.write(json.dumps(c, sort_keys=True) + "\n")
        for fi, v1, v2, v2_pred, v2_subj in v2_facts:
            v2_doc_facts = list(doc_facts)
            phrase, subj, _ = v2_doc_facts[fi]
            v2_doc_facts[fi] = (phrase, subj, v2)
            v2_text = _doc_body(rng, subject, v2_doc_facts, subject)
            with open(os.path.join(out_dir, "v2", "doc-%03d.md" % i),
                      "w") as f:
                f.write(v2_text)
            cand = {"subject": v2_subj, "predicate": v2_pred,
                    "value": int(v2) if v2.isdigit() else v2,
                    "locator": "s:" + _fact_sentence(phrase, v2_subj,
                                                   v2)[:-1],
                    "authority": "doc"}
            with open(os.path.join(out_dir, "candidates_v2",
                                   "doc-%03d.jsonl" % i), "w") as f:
                f.write(json.dumps(cand, sort_keys=True) + "\n")

    proj = ["# PROJECT summary (hand maintained)", "",
            "Curated facts for the service fleet. This file is updated by",
            "hand, so corrections to source docs are not always reflected.",
            "",
            "| fact | subject | predicate | value | source |",
            "| --- | --- | --- | --- | --- |"]
    for fid, subj, pred, val, src in summary_rows:
        proj.append("| %s | %s | %s | %s | %s |" % (fid, subj, pred, val,
                                                   src))
    with open(os.path.join(out_dir, "PROJECT.md"), "w") as f:
        f.write("\n".join(proj) + "\n")

    with open(os.path.join(out_dir, "labels.json"), "w") as f:
        json.dump(labels, f, indent=2, sort_keys=True)
        f.write("\n")
    return labels


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", required=True)
    ap.add_argument("--docs", type=int, default=40)
    ap.add_argument("--facts-per-doc", type=int, default=4)
    ap.add_argument("--seed", type=int, default=DEFAULT_SEED)
    args = ap.parse_args()
    labels = generate(args.out, args.docs, args.facts_per_doc, args.seed)
    n_corr = sum(1 for x in labels["facts"] if x["correction_case"])
    n_conf = sum(1 for x in labels["facts"] if x["conflict_case"])
    print("docs=%d facts=%d corrections=%d conflicts=%d -> %s"
          % (labels["docs"], len(labels["facts"]), n_corr, n_conf,
             args.out))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
