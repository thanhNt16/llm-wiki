#!/usr/bin/env python3
"""Perf ceilings (SP1). Fast tier runs in CI-seconds; --deep covers 5k/10k.
Exit 0 iff every ceiling holds. Results appended to evals/perf/results.json."""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))
sys.path.insert(0, HERE)

import bench  # noqa: E402

CEILINGS_FAST = {          # 1,000-claim corpus; RSS medians ×1.3 (SP2a Task 10)
    "apply_s": 5.0,
    "query_s": 1.0,
    "graph_s": 2.0,
    "apply_rss_mb": 30.0,
    "query_rss_mb": 30.0,
    "graph_rss_mb": 30.0,
}
CEILINGS_DEEP = {          # 5k, then 10k; RSS measured once ×1.3, rounded up
    5000: {"apply_s": 30.0, "graph_s": 10.0, "query_s": 2.0,
           "apply_rss_mb": 40.0, "query_rss_mb": 60.0, "graph_rss_mb": 65.0},
    10000: {"apply_s": 60.0, "graph_s": 20.0, "query_s": 3.0,
            "apply_rss_mb": 65.0, "query_rss_mb": 100.0, "graph_rss_mb": 105.0},
}


def run(n_docs):
    m = bench.run_corpus(n_docs)
    return m


def main():
    deep = "--deep" in sys.argv
    results = {"fast": run(100)}
    fails = []
    for metric, ceiling in CEILINGS_FAST.items():
        v = results["fast"][metric]
        unit = "MB" if metric.endswith("_rss_mb") else "s"
        print("fast %-14s %8.3f %-2s (ceiling %6.1f)  %s"
              % (metric, v, unit, ceiling, "OK" if v <= ceiling else "FAIL"))
        if v > ceiling:
            fails.append(("fast", metric, v, ceiling))
    if deep:
        for claims, ceilings in CEILINGS_DEEP.items():
            m = run(claims // 10)
            results["%d" % claims] = m
            for metric, ceiling in ceilings.items():
                unit = "MB" if metric.endswith("_rss_mb") else "s"
                print("%-5d %-14s %8.3f %-2s (ceiling %6.1f)  %s"
                      % (claims, metric, m[metric], unit, ceiling,
                         "OK" if m[metric] <= ceiling else "FAIL"))
                if m[metric] > ceiling:
                    fails.append((claims, metric, m[metric], ceiling))
    path = os.path.join(HERE, "results.json")
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
           "host": os.uname().machine, "deep": deep, "results": results}
    with open(path, "a") as f:
        f.write(json.dumps(rec) + "\n")
    if fails:
        print("PERF GATE FAIL:", fails)
        return 1
    print("perf gates: all OK (results appended to evals/perf/results.json)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
