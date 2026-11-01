"""Timed engine ops over a generated corpus. Mutating ops are single-shot."""
import json
import os
import resource
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))
sys.path.insert(0, HERE)

import gen_corpus  # noqa: E402
from wikicore import compile as wc_compile, graphdata, query  # noqa: E402


def _maxrss_mb():
    """Peak RSS of this process in MB. macOS ru_maxrss is bytes, Linux KiB."""
    v = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return v / (1024.0 * 1024.0) if sys.platform == "darwin" else v / 1024.0


# Coarse semantics, deliberate: ops run in one process, so ru_maxrss is a
# monotonic high-water mark — each *_rss_mb is the process peak at that op's
# end, not an isolated per-op delta. It still bounds "op never pushes the
# engine past X MB", which is the ceiling's job.
_RSS = {}


def _t(fn, rss_key=None):
    t0 = time.perf_counter()
    out = fn()
    dt = time.perf_counter() - t0
    if rss_key is not None:
        _RSS[rss_key] = round(_maxrss_mb(), 1)
    return dt, out



def _staged_runs(wiki):
    # Same layout _staging_runs()/cmd_reconcile_prepare enumerate:
    # stage_candidates parks <run_id>/compile.json under .state/staging.
    base = wiki.p(".state", "staging")
    if not os.path.isdir(base):
        return []
    return sorted(d for d in os.listdir(base)
                  if d.startswith("run_")
                  and os.path.isdir(os.path.join(base, d)))


def run_corpus(n_docs, per_doc=10, keep_root=None):
    """keep_root: a directory path to (re)use and leave behind for
    inspection; when None the corpus root is a cleaned-up tempdir."""
    import shutil
    import tempfile

    root = keep_root or tempfile.mkdtemp(prefix="llmwiki-perf-")
    try:
        return _run_corpus(root, n_docs, per_doc)
    finally:
        if keep_root is None:
            shutil.rmtree(root, ignore_errors=True)


def _run_corpus(root, n_docs, per_doc):
    ingest_s, wiki = _t(lambda: gen_corpus.build(root, n_docs, per_doc),
                        rss_key="ingest")
    runs = _staged_runs(wiki)
    prepare_s = 0.0
    classifications = {}
    for rid in runs:
        def _prep(rid=rid):
            prep = wc_compile.reconcile_prepare(wiki, rid)
            classifications[rid] = [
                {"index": c["index"],
                 "relationship": "UNRELATED",   # worst case: everything new
                 "target_claim_id": None, "valid_from": None, "valid_to": None,
                 "authority": {"type": "authoritative_source"}}
                for c in prep["comparisons"]]
            return prep
        dt, _ = _t(_prep, rss_key="prepare")
        prepare_s += dt

    def _apply():
        # reconcile_apply consumes one run's classifications at a time; the
        # last-committed revision is the base for the next
        for rid in runs:
            wc_compile.reconcile_apply(wiki, rid, classifications[rid],
                                       wiki.revision())
    apply_s, _ = _t(_apply, rss_key="apply")
    query_s, _ = _t(lambda: query.prepare(wiki, "who owns acme.svc00"),
                    rss_key="query")
    graph_s, payload = _t(lambda: graphdata.graph_payload(wiki),
                          rss_key="graph")
    claims = sum(1 for f in os.listdir(wiki.p("claims"))
                 if f.startswith("claim_") and f.endswith(".json"))
    out = {"docs": n_docs, "claims": claims,
           "ingest_s": round(ingest_s, 3), "ingest_rss_mb": _RSS["ingest"],
           "prepare_s": round(prepare_s, 3), "prepare_rss_mb": _RSS["prepare"],
           "apply_s": round(apply_s, 3), "apply_rss_mb": _RSS["apply"],
           "query_s": round(query_s, 3), "query_rss_mb": _RSS["query"],
           "graph_s": round(graph_s, 3), "graph_rss_mb": _RSS["graph"],
           "bytes_graph": len(json.dumps(payload))}
    _RSS.clear()
    return out
