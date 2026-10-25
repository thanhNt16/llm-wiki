#!/usr/bin/env python3
"""Perf baselines + >10x budget gate (cheap CI tier).

Usage:
    python3 evals/perf/check_budgets.py --record   # measure on HEAD, write baselines.json
    python3 evals/perf/check_budgets.py            # re-measure; exit 1 ONLY on >10x regression

Metrics (baselines.json):
    lint_sweep_300_pages_s  wall time of doctor.run() over a synthesized
                            ~300-page wiki (temp dir, write_page/Transaction
                            pattern mirrored from skill/llm-wiki/scripts/tests/)
    similar_pages_300_s     wall time of a synthesize.similar_pages() sweep:
                            one call per page subject over the same 300-page wiki
    pack_tokens_avg         metrics.context() over the standard golden-corpus
                            wiki built via evals/benchmarks/driver.build_wiki;
                            falls back to the pinned constant when the metric
                            cannot be produced
    build_wall_s            wall time of driver.build_wiki (init -> ingest ->
                            stage -> reconcile -> correction -> build-pages ->
                            verify) over evals/golden-corpus

Intended as the CI cheap tier: wiring into evals/deterministic/run_gates.py
(gates script calls this last when baselines.json exists) is deferred to the
docs task; invoke manually or record baselines after perf-relevant changes.
Exit codes: 0 within budget, 1 on >10x regression, 2 on missing/corrupt
baselines or measurement failure.
"""
import argparse
import json
import os
import shutil
import sys
import tempfile
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))))
ENGINE_SCRIPTS = os.path.join(REPO_ROOT, "skill", "llm-wiki", "scripts")
BENCH_DIR = os.path.join(REPO_ROOT, "evals", "benchmarks")
BASELINES = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                         "baselines.json")
WIKI_PY = os.path.join(ENGINE_SCRIPTS, "wiki.py")
CORPUS = os.path.join(REPO_ROOT, "evals", "golden-corpus")

# Spec-pinned pack-token average: kept as the fallback when the context-pack
# metric cannot be produced (e.g. no resolvable claims in the built wiki).
PACK_TOKENS_FALLBACK = 263.8
REGRESSION_FACTOR = 10.0
PAGE_COUNT = 300

for _p in (ENGINE_SCRIPTS, BENCH_DIR):
    if _p not in sys.path:
        sys.path.insert(0, _p)

from wikicore import compile as wc_compile  # noqa: E402
from wikicore import claims, doctor, sources, synthesize  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
import driver  # noqa: E402  evals/benchmarks driver (subprocess CLI wrapper)
import metrics  # noqa: E402


def _add_accepted_claim(wiki, subject, predicate, value):
    """Mirror tests/test_doctor.py::add_accepted_claim."""
    r = sources.ingest(wiki, "text", "perf-src-%s-%s" % (subject, predicate),
                       b"body")
    cand = {
        "id": "auto", "subject": subject, "predicate": predicate,
        "value": value, "scope": {}, "valid_from": None, "valid_to": None,
        "recorded_at": "2026-09-15T00:00:00Z", "status": "candidate",
        "proposed_by": "agent",
        "authority": {"type": "authoritative_source"},
        "evidence": [{"source_id": r["source_id"], "source_version": 1,
                      "locator": {"type": "text", "value": "x"}}],
        "supersedes": [],
    }
    rid = wc_compile.stage_candidates(
        wiki, r["source_id"], 1, [cand],
        {"source_id": r["source_id"], "source_version": 1,
         "coverage": {"text": "complete"}, "warnings": [],
         "parser": {"name": "perf", "version": "1"}})
    wc_compile.reconcile_apply(
        wiki, rid,
        [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
          "valid_from": None, "valid_to": None,
          "authority": {"type": "authoritative_source"}}],
        wiki.revision())
    return claims.list_claims(wiki, subject=subject)[0]


def _page_md(ptype, subject, title, link):
    """Frontmatter satisfying write_page's validation (ISO created, subject
    matching the slug); body carries one [[claim_...]] link so deps auto-derive
    for non-question kinds."""
    return ("---\ntype: %s\nsubject: %s\ntitle: %s\n"
            "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
            % (ptype, subject, title, title, link))


def _build_300_page_wiki(root):
    """Synthesize a ~300-page wiki; returns (wiki, [(kind, subject, title)])."""
    wiki = Wiki(root)
    init_wiki(wiki, "perf-300")
    claim = _add_accepted_claim(wiki, "perf anchor", "count", 1)
    link = "[[%s]]" % claim["id"]
    subjects = []
    # 100 concepts + 100 procedures cite the claim (deps auto-derived from the
    # [[claim]] link); 100 questions may carry empty deps. Subjects use a shared
    # "perf page" prefix so sweeps exercise a token-sharing corpus without
    # tripping the near-duplicate write guard (Jaccard 1/3 < 0.60).
    for i in range(PAGE_COUNT // 3):
        subjects.append(("concepts", "perf page concept %04d" % i,
                         "Perf Page Concept %04d" % i, link))
        subjects.append(("procedures", "perf page procedure %04d" % i,
                         "Perf Page Procedure %04d" % i, link))
        subjects.append(("questions", "perf page question %04d" % i,
                         "Perf Page Question %04d" % i, ""))
    for kind, subject, title, link_ in subjects:
        rel = "wiki/%s/%s.md" % (kind, synthesize.slugify(subject))
        synthesize.write_page(
            wiki, rel, _page_md(synthesize.KIND_TYPE[kind], subject, title,
                                link_),
            None, wiki.revision(), allow_new=True)
    return wiki, subjects


def _measure_300_page_sweeps():
    """Build the 300-page wiki in a temp dir; time doctor.run and a
    similar_pages sweep (one call per page subject)."""
    root = tempfile.mkdtemp(prefix="llmwiki-perf-")
    try:
        wiki, subjects = _build_300_page_wiki(root)
        t0 = time.perf_counter()
        doctor.run(wiki)
        lint_s = time.perf_counter() - t0
        t0 = time.perf_counter()
        for kind, subject, title, _link in subjects:
            synthesize.similar_pages(wiki, kind, subject, title)
        sim_s = time.perf_counter() - t0
        return lint_s, sim_s
    finally:
        shutil.rmtree(root, ignore_errors=True)


def _measure_build_and_pack():
    """Time driver.build_wiki over golden-corpus; then metrics.context for the
    pack-token average (fallback constant when unavailable)."""
    engine = driver.Engine(WIKI_PY)
    root = tempfile.mkdtemp(prefix="llmwiki-perf-bench-")
    try:
        t0 = time.perf_counter()
        build = driver.build_wiki(engine, CORPUS, root)
        build_s = time.perf_counter() - t0
        if not build.get("ok"):
            print("warning: standard build not fully ok; build_wall_s "
                  "measured anyway", file=sys.stderr)
        ctx = metrics.context(engine, root, CORPUS)
        pack = ctx.get("pack_tokens_avg") or PACK_TOKENS_FALLBACK
        return build_s, pack
    finally:
        shutil.rmtree(root, ignore_errors=True)


def measure():
    build_s, pack = _measure_build_and_pack()
    lint_s, sim_s = _measure_300_page_sweeps()
    return {
        "lint_sweep_300_pages_s": round(lint_s, 3),
        "similar_pages_300_s": round(sim_s, 3),
        "pack_tokens_avg": pack,
        "build_wall_s": round(build_s, 3),
    }


def _print_table(measured, baselines=None):
    rows = []
    for key in ("lint_sweep_300_pages_s", "similar_pages_300_s",
                "pack_tokens_avg", "build_wall_s"):
        m = measured.get(key)
        b = baselines.get(key) if baselines else None
        ratio = (m / b) if (b and isinstance(m, (int, float))) else None
        rows.append((key,
                     "%.3f" % m if isinstance(m, (int, float)) else str(m),
                     "%.3f" % b if isinstance(b, (int, float)) else "-",
                     "%.2fx" % ratio if ratio is not None else "-"))
    width = max(len(r[0]) for r in rows)
    print("%-*s  %12s  %12s  %8s" % (width, "metric", "measured", "baseline",
                                     "ratio"))
    for key, m, b, ratio in rows:
        print("%-*s  %12s  %12s  %8s" % (width, key, m, b, ratio))


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--record", action="store_true",
                    help="measure on HEAD and write baselines.json")
    args = ap.parse_args(argv)

    measured = measure()

    if args.record:
        with open(BASELINES, "w") as f:
            json.dump(measured, f, indent=2)
            f.write("\n")
        _print_table(measured)
        print("wrote %s" % os.path.relpath(BASELINES, REPO_ROOT))
        return 0

    if not os.path.isfile(BASELINES):
        print("baselines missing: %s — run with --record first" % BASELINES,
              file=sys.stderr)
        return 2
    try:
        with open(BASELINES) as f:
            baselines = json.load(f)
    except (OSError, ValueError) as e:
        print("baselines corrupt: %s" % e, file=sys.stderr)
        return 2

    _print_table(measured, baselines)
    failed = []
    for key, m in measured.items():
        b = baselines.get(key)
        if isinstance(b, (int, float)) and b > 0 and m > b * REGRESSION_FACTOR:
            failed.append((key, m, b))
    if failed:
        for key, m, b in failed:
            print("REGRESSION >10x: %s measured %.3f vs baseline %.3f"
                  % (key, m, b), file=sys.stderr)
        return 1
    print("perf budgets OK (all metrics within 10x of baseline)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
