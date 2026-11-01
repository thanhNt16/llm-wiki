#!/usr/bin/env python3
"""Run the 24-question battery against a wiki; print + write metrics.
Deterministic: same wiki + same ranker -> same numbers.

Metrics: recall@1 and recall@5 over expected_subjects, plus per-kind
breakdown. `ranker` comes from query.prepare itself ("bm25" once the FTS
cache is live, "legacy" before/fallback) so every result file states which
scorer produced it."""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import query  # noqa: E402
from wikicore.store import Wiki  # noqa: E402

K = 5


def main(wiki_root, out_json=None):
    w = Wiki(wiki_root)
    questions = json.load(open(os.path.join(HERE, "questions.json")))
    per_q, top1, topk, rankers = [], 0, 0, set()
    for q in questions:
        r = query.prepare(w, q["question"])
        rankers.add(r.get("ranker", "legacy"))
        subj = [m["subject"] for m in r["matches"]]
        h1 = bool(subj) and subj[0] in q["expected_subjects"]
        hk = any(s in q["expected_subjects"] for s in subj[:K])
        top1 += h1
        topk += hk
        per_q.append({"qid": q["qid"], "kind": q["kind"], "hit_top1": h1,
                      "hit_topk": hk, "returned_subjects": subj[:K]})
    by_kind = {}
    for q in per_q:
        d = by_kind.setdefault(q["kind"], {"n": 0, "top1": 0, "topk": 0})
        d["n"] += 1
        d["top1"] += q["hit_top1"]
        d["topk"] += q["hit_topk"]
    out = {"ranker": "+".join(sorted(rankers)),
           "per_q": per_q,
           "recall_top1": top1 / len(questions),
           "recall_topk": topk / len(questions),
           "by_kind": {k: {"recall_top1": v["top1"] / v["n"],
                           "recall_topk": v["topk"] / v["n"]}
                       for k, v in sorted(by_kind.items())}}
    if out_json:
        with open(out_json, "w") as f:
            json.dump(out, f, indent=2)
    print("ranker=%s  recall@1 %.3f  recall@%d %.3f" % (
        out["ranker"], out["recall_top1"], K, out["recall_topk"]))
    for k, v in sorted(out["by_kind"].items()):
        print("  %-5s top1 %.2f  top%d %.2f" % (k, v["recall_top1"], K, v["recall_topk"]))
    return out


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None)
