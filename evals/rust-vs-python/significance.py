#!/usr/bin/env python3
"""Paired one-sided permutation tests for the rust-vs-python quality lane.

    python3 significance.py [--dir DIR] [--permutations N] [--seed S] \
        [--metric KEY] [--out quality.json]

Reads quality-rust.json / quality-python.json / quality-baseline.json from
--dir (default: this script's directory), extracts the per-question
correctness vector for each lane (default metric: "exact_match"), aligns
by fact_id, and runs a paired permutation test per lane pair: under the
null, swapping each paired score is equiprobable, so we randomly swap
per-pair values --permutations times (fixed seed) and compare the observed
difference in means against the permuted distribution.

Reports, per pair, p_worse and p_better following
/tmp/crow-llmwiki-v2/benchmarks/verdict.py conventions: the statistic is
oriented so that "better" = higher mean of the first lane.

Can also be imported: paired_permutation_p(a, b, seed, permutations).

Stdlib only, Python >= 3.9.
"""
import argparse
import json
import os
import random

SEED = 20261007
PERMUTATIONS = 20000
PAIRS = (("rust", "python"), ("rust", "baseline"), ("python", "baseline"))
# metric -> +1 if higher is better, -1 if lower is better
METRICS = {"exact_match": 1, "correct_current": 1, "citation_ok": 1,
           "stale_answer": -1, "context_bytes": -1}


def paired_permutation_p(a, b, seed=SEED, permutations=PERMUTATIONS):
    """Paired one-sided permutation test on per-item scores.

    a, b: equal-length score vectors (aligned per item). Under the null
    the within-pair assignment is exchangeable, so each permutation swaps
    a[i]/b[i] independently at random. Returns (p_worse, p_better) where
    p_better = fraction of permuted diffs >= observed - eps (a > b).
    """
    if len(a) != len(b):
        raise ValueError("paired vectors differ in length: %d vs %d"
                         % (len(a), len(b)))
    n = len(a)
    if n == 0:
        return None, None
    obs = sum(a) / n - sum(b) / n
    diffs = [a[i] - b[i] for i in range(n)]
    rng = random.Random(seed)
    ge = le = 0
    for _ in range(permutations):
        d = 0.0
        for i in range(n):
            if rng.random() < 0.5:
                d += diffs[i]
            else:
                d -= diffs[i]
        d /= n
        if d >= obs - 1e-12:
            ge += 1
        if d <= obs + 1e-12:
            le += 1
    # obs>0 -> p_worse=P(D<=obs), p_better=P(D>=obs); consistent with
    # verdict.py: statistic oriented in the 'better' direction.
    return le / permutations, ge / permutations


def metric_vector(lane_json, metric):
    """fact_id -> numeric per-question value for `metric`.

    Booleans map to 0/1; numbers pass through. None values are skipped.
    """
    out = {}
    for q in lane_json["per_question"]:
        v = q.get(metric)
        if v is None:
            continue
        out[q["fact_id"]] = (1.0 if v else 0.0) if isinstance(v, bool) \
            else float(v)
    return out


def run(lane_dir, permutations=PERMUTATIONS, seed=SEED):
    lanes = {}
    for lane in ("rust", "python", "baseline"):
        path = os.path.join(lane_dir, "quality-%s.json" % lane)
        if not os.path.isfile(path):
            continue
        with open(path) as f:
            lanes[lane] = json.load(f)
    results = {}
    for a, b in PAIRS:
        pair = "%s_vs_%s" % (a, b)
        if a not in lanes or b not in lanes:
            results[pair] = {"status": "n/a",
                             "reason": "missing lane json for %s or %s"
                                       % (a, b)}
            continue
        results[pair] = {"status": "ok", "tests": {}}
        for metric, sign in sorted(METRICS.items()):
            va = metric_vector(lanes[a], metric)
            vb = metric_vector(lanes[b], metric)
            keys = sorted(set(va) & set(vb))
            if not keys:
                results[pair]["tests"][metric] = {
                    "status": "n/a", "reason": "no shared fact_ids"}
                continue
            pa = [sign * va[k] for k in keys]
            pb = [sign * vb[k] for k in keys]
            p_worse, p_better = paired_permutation_p(pa, pb, seed,
                                                   permutations)
            results[pair]["tests"][metric] = {
                "status": "ok",
                "direction": "higher_is_better" if sign > 0
                            else "lower_is_better",
                "n_questions": len(keys),
                "mean_%s" % a: round(sum(va[k] for k in keys)
                                     / len(keys), 6),
                "mean_%s" % b: round(sum(vb[k] for k in keys)
                                     / len(keys), 6),
                "p_first_is_worse": round(p_worse, 6),
                "p_first_is_better": round(p_better, 6),
            }
        results[pair]["seed"] = seed
        results[pair]["permutations"] = permutations

    # token savings: wiki context bytes vs baseline bytes, per pair and
    # metric-free (bytes are in the same unit on both sides).
    comparison = {}
    if "baseline" in lanes:
        base_bytes = {q["fact_id"]: q["context_bytes"]
                      for q in lanes["baseline"]["per_question"]}
        for lane in ("rust", "python"):
            if lane not in lanes:
                continue
            savings = []
            for q in lanes[lane]["per_question"]:
                bb = base_bytes.get(q["fact_id"])
                if bb:
                    savings.append(1.0 - q["context_bytes"] / bb)
            comparison["%s_vs_baseline" % lane] = {
                "token_savings_pct": (
                    round(sum(savings) / len(savings), 6)
                    if savings else None),
                "token_savings_n": len(savings),
            }
    return {"significance": results, "comparison": comparison}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dir", default=os.path.dirname(
        os.path.abspath(__file__)))
    ap.add_argument("--permutations", type=int, default=PERMUTATIONS)
    ap.add_argument("--seed", type=int, default=SEED)
    ap.add_argument("--out", default=None,
                    help="merge results into this quality.json "
                         "(default: <dir>/quality.json)")
    args = ap.parse_args()
    res = run(args.dir, args.permutations, args.seed)

    out = args.out or os.path.join(args.dir, "quality.json")
    merged = {"significance": res["significance"],
              "comparison": res["comparison"], "lanes": {}}
    for lane in ("rust", "python", "baseline"):
        path = os.path.join(args.dir, "quality-%s.json" % lane)
        if os.path.isfile(path):
            with open(path) as f:
                merged["lanes"][lane] = json.load(f)
    with open(out, "w") as f:
        json.dump(merged, f, indent=2, sort_keys=True)
        f.write("\n")
    for pair, r in sorted(res["significance"].items()):
        if r["status"] != "ok":
            print("%s: %s" % (pair, r["reason"]))
            continue
        for metric, t in sorted(r["tests"].items()):
            if t["status"] == "ok":
                print("%s/%s: p_better=%.4f p_worse=%.4f (n=%d)"
                      % (pair, metric, t["p_first_is_better"],
                         t["p_first_is_worse"], t["n_questions"]))
    for pair, c in sorted(res["comparison"].items()):
        print("%s: token_savings=%s" % (pair, c["token_savings_pct"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
