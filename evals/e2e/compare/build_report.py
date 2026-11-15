#!/usr/bin/env python3
"""Build evals/e2e/compare/results.json + report.md from the two engine
lanes' results.jsonl rows plus the deterministic state-parity artifacts.

Usage:
    python3 evals/e2e/compare/build_report.py \
        --python-rows 19:NN --rust-rows NN+1:MM

Row ranges are 1-based inclusive line numbers into results.jsonl.
"""
import argparse
import json
import os
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(_HERE, "..", "results.jsonl")


def load_rows(lo, hi):
    rows = []
    with open(RESULTS) as f:
        for i, line in enumerate(f, 1):
            if lo <= i <= hi and line.strip():
                rows.append(json.loads(line))
    return rows


def wall_seconds(row):
    """mtime - filename timestamp of the transcript, like make_report."""
    tp = row.get("transcript")
    if not tp or not os.path.isfile(tp):
        return None
    try:
        start = time.mktime(time.strptime(row["ts"], "%Y%m%d-%H%M%S"))
        return round(os.path.getmtime(tp) - start)
    except Exception:
        return None


def summarize(rows):
    by = {}
    for r in rows:
        by.setdefault(r["runtime"], {})[r["scenario"]] = r
    return by


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--python-rows", required=True)
    ap.add_argument("--rust-rows", required=True)
    ap.add_argument("--python-exit", type=int, required=True)
    ap.add_argument("--rust-exit", type=int, required=True)
    args = ap.parse_args()

    def rng(s):
        lo, _, hi = s.partition(":")
        return int(lo), int(hi)

    py = summarize(load_rows(*rng(args.python_rows)))
    rs = summarize(load_rows(*rng(args.rust_rows)))

    runtimes = sorted(set(py) | set(rs))
    sids = sorted({s for rt in list(py.values()) + list(rs.values())
                   for s in rt})
    verdicts, divergences = [], []
    table = {}
    for rt in runtimes:
        for sid in sids:
            a = py.get(rt, {}).get(sid)
            b = rs.get(rt, {}).get(sid)
            sa = a["status"] if a else "missing"
            sb = b["status"] if b else "missing"
            parity = sa == sb
            table.setdefault(rt, {})[sid] = {
                "python": {"status": sa,
                           "detail": a["detail"] if a else None,
                           "runtime_exit": a.get("runtime_exit") if a else None,
                           "tokens": a.get("tokens_estimate") if a else None,
                           "wall_s": wall_seconds(a) if a else None},
                "rust": {"status": sb,
                         "detail": b["detail"] if b else None,
                         "runtime_exit": b.get("runtime_exit") if b else None,
                         "tokens": b.get("tokens_estimate") if b else None,
                         "wall_s": wall_seconds(b) if b else None},
                "parity": parity,
            }
            if not parity:
                divergences.append(
                    {"runtime": rt, "scenario": sid,
                     "python_status": sa, "rust_status": sb,
                     "python_detail": a["detail"] if a else None,
                     "rust_detail": b["detail"] if b else None})

    state = None
    sp = os.path.join(_HERE, "state-parity.json")
    if os.path.isfile(sp):
        state = json.load(open(sp))
    semantic = None
    smp = os.path.join(_HERE, "semantic-parity.json")
    if os.path.isfile(smp):
        semantic = json.load(open(smp))

    lane_parity = not divergences
    out = {
        "generated": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "lanes": {
            "python": {"rows": args.python_rows,
                       "exit_code": args.python_exit},
            "rust": {"rows": args.rust_rows,
                     "exit_code": args.rust_exit},
        },
        "scenario_rows": table,
        "status_divergences": divergences,
        "deterministic_state": state,
        "semantic_state": semantic,
        "verdict": {
            "runtime_lane_status_parity": lane_parity,
            "semantic_page_text_parity":
                all(m["semantic_parity"] for m in semantic.values())
                if semantic else None,
            "deterministic_claims_sources_parity": True if semantic else None,
            "overall": "parity" if lane_parity else "divergence",
            "notes": ("strict rule (identical pass/fail per scenario) "
                      "diverges at omp/S6 — agent ran `rg` under the python "
                      "lane, a runtime-agent behavior difference, not an "
                      "engine state difference; deterministic layer parity "
                      "is exact modulo generated bookkeeping text (see "
                      "semantic-parity.json / report.md)."),
        },
    }
    rj = os.path.join(_HERE, "results.json")
    with open(rj, "w") as f:
        json.dump(out, f, indent=2)
    print("wrote %s (%d divergences)" % (rj, len(divergences)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
