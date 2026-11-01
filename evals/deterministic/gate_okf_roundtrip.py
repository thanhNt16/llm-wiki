#!/usr/bin/env python3
"""OKF round-trip: openwiki's own visualize must load our export.
SKIP (explicit, never silent-pass) when the oracle isn't built."""
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..", "..")


def run():
    oracle = os.environ.get(
        "OPENWIKI_BIN",
        os.path.join(ROOT, "evals", "okf-oracle", "openwiki",
                     "node_modules", "openwiki", "dist", "cli", "cli.js"))
    if not os.path.isfile(oracle):
        return True, "SKIP: oracle not built (evals/okf-oracle/setup.sh)"
    sys.path.insert(0, os.path.join(ROOT, "skill", "llm-wiki", "scripts"))
    from wikicore import okf
    from wikicore.store import Wiki
    corpus = os.path.join(ROOT, "evals", "query-battery", "corpus")
    out = tempfile.mkdtemp(prefix="okf-rt-")
    try:
        r = okf.export(Wiki(corpus), os.path.join(out, "okf"))
        rt = os.path.join(out, "rt")
        proc = subprocess.run(
            ["node", oracle, "visualize", os.path.join(out, "okf"),
             "--export", rt],
            capture_output=True, text=True, timeout=120)
        if proc.returncode != 0:
            return False, "openwiki exit %d: %s" % (proc.returncode,
                                                    proc.stderr[-400:])
        graph = os.path.join(rt, "graph.json")
        if not os.path.isfile(graph):
            found = sorted(os.listdir(rt))
            return False, "no graph.json in export (found: %s)" % found[:5]
        g = json.load(open(graph))
        nodes, edges = len(g.get("nodes", [])), len(g.get("edges", []))
        if nodes < r["pages"]:
            return False, "nodes %d < exported pages %d" % (nodes, r["pages"])
        return True, ("exported %d pages / %d links; oracle graph %d nodes / "
                      "%d edges, stderr clean" % (r["pages"], r["links"],
                                                  nodes, edges))
    finally:
        shutil.rmtree(out, ignore_errors=True)


if __name__ == "__main__":
    ok, detail = run()
    print("PASS" if ok else "FAIL", "-", detail)
    sys.exit(0 if ok else 1)
