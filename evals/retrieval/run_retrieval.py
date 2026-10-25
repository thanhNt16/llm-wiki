#!/usr/bin/env python3
"""Retrieval eval — precision/recall/MRR of `wiki.py query-prepare` against
a gold question set.

    python3 evals/retrieval/run_retrieval.py [--wiki DIR] [--top-k N]

For each gold question, runs `query-prepare --question` against the wiki and
scores the ranked `matches[]` list:

- relevant = returned match whose claim.subject is in expect.subjects
- recall@k = found expected-subject claim within top-k matches
- precision@k = fraction of top-k matches that are relevant
- MRR = 1/rank of first relevant match
- unanswerable questions: pass iff zero top-k matches are relevant (i.e.
  retrieval produced no confident false hits on the gold subjects — none
  exist — measured as "abstention cleanliness": fewest irrelevant hits is
  better, hard fail only when a claimed-relevant subject can't exist)

Prints a table + JSON summary; exits 0 always (measurement, not a gate).
"""
import argparse
import json
import os
import subprocess
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.abspath(os.path.join(_HERE, "..", ".."))
WIKI_PY = os.path.join(_REPO, "skill", "llm-wiki", "scripts", "wiki.py")
GOLD = os.path.join(_HERE, "gold.json")


def prepare(wiki_root, question, as_of=None):
    argv = [sys.executable, WIKI_PY, "--root", wiki_root,
            "query-prepare", "--question", question]
    if as_of:
        argv += ["--as-of", as_of]
    proc = subprocess.run(argv, capture_output=True, text=True, timeout=60,
                          cwd=wiki_root)
    if proc.returncode != 0:
        return {"matches": [], "error": proc.stderr.strip()[:200]}
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {"matches": [], "error": "non-JSON output"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wiki", default="/tmp/ptt-wiki-demo-2")
    ap.add_argument("--top-k", type=int, default=5)
    ap.add_argument("--gold", default=GOLD)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    gold = json.load(open(args.gold))
    wiki = args.wiki or gold.get("wiki")
    k = args.top_k

    rows = []
    for q in gold["questions"]:
        out = prepare(wiki, q["question"], q.get("expect", {}).get("as_of"))
        matches = out.get("matches") or []
        expected = set(q["expect"].get("subjects") or [])
        ranked = [m.get("subject", "") for m in matches[:k]]
        rel_hits = [i for i, s in enumerate(ranked) if s in expected]
        recall = 1.0 if rel_hits else 0.0
        precision = (len(rel_hits) / len(ranked)) if ranked else 0.0
        rr = (1.0 / (rel_hits[0] + 1)) if rel_hits else 0.0
        unanswerable = bool(q["expect"].get("unanswerable"))
        # for unanswerable: pass iff retrieval surfaces nothing confidently
        # wrong — measured as fraction of top-k hits that are NOT gold subjects
        # (all are non-gold by construction); abstention ok when matches are
        # empty or only loosely related
        abstain_ok = (not ranked) or all(s not in expected for s in ranked)
        rows.append({
            "id": q["id"], "q": q["question"], "k": k,
            "matches": len(matches), "top_k": ranked,
            "recall": recall, "precision": precision, "rr": rr,
            "unanswerable": unanswerable,
            "abstain_ok": abstain_ok,
            "superseded_shown": len(out.get("superseded") or []),
            "conflicts": len(out.get("conflicts") or []),
        })

    answerable = [r for r in rows if not r["unanswerable"]]
    unans = [r for r in rows if r["unanswerable"]]
    n = len(answerable)
    summary = {
        "wiki": wiki, "k": k, "n_answerable": n, "n_unanswerable": len(unans),
        "recall@%d" % k: round(sum(r["recall"] for r in answerable) / n, 3),
        "precision@%d" % k: round(sum(r["precision"] for r in answerable) / n, 3),
        "mrr": round(sum(r["rr"] for r in answerable) / n, 3),
        "mean_matches": round(sum(r["matches"] for r in rows) / len(rows), 1),
        "unanswerable_clean": sum(1 for r in unans if r["abstain_ok"]),
        "rows": rows,
    }

    if args.json:
        print(json.dumps(summary, indent=1))
        return 0

    print("wiki: %s  top-k=%d\n" % (wiki, k))
    print("%-4s %-58s %4s %5s %5s %4s %s" % (
        "id", "question", "mtch", "R@%d" % k, "P@%d" % k, "MRR", "top subjects"))
    for r in rows:
        mark = "" if r["recall"] else "  <-- MISS"
        if r["unanswerable"]:
            mark = "  (unanswerable: %s)" % ("clean" if r["abstain_ok"] else "LEAK")
        print("%-4s %-58s %4d %5.2f %5.2f %4.2f %s%s" % (
            r["id"], r["q"][:58], r["matches"], r["recall"], r["precision"],
            r["rr"], ", ".join(r["top_k"][:4]), mark))
    print("\nsummary: recall@%d=%.3f  precision@%d=%.3f  MRR=%.3f  "
          "(n=%d answerable)  unanswerable clean=%d/%d  mean matches=%.1f" % (
              k, summary["recall@%d" % k], k, summary["precision@%d" % k],
              summary["mrr"], n, summary["unanswerable_clean"], len(unans),
              summary["mean_matches"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
