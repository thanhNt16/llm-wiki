"""Timed engine ops over a generated corpus. Mutating ops are single-shot.

--engine rust drives the identical op sequence through the llm-wiki CLI
binary (no wikicore import): init -> ingest+stage per doc ->
reconcile-prepare/apply -> query-prepare -> graph-data. RSS columns use
RUSAGE_CHILDREN (max child peak) instead of RUSAGE_SELF there — coarse,
but the same op/metric contract."""
import json
import os
import resource
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))
sys.path.insert(0, HERE)

import gen_corpus  # noqa: E402

# wikicore is imported lazily inside _run_corpus so the rust lane never
# loads the python engine it is benchmarking against.



def _maxrss_mb():
    """Peak RSS of this process in MB. macOS ru_maxrss is bytes, Linux KiB."""
    v = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return v / (1024.0 * 1024.0) if sys.platform == "darwin" else v / 1024.0


# Coarse semantics, deliberate: ops run in one process, so ru_maxrss is a
# monotonic high-water mark — each *_rss_mb is the process peak at that op's
# end, not an isolated per-op delta. It still bounds "op never pushes the
# engine past X MB", which is the ceiling's job.
# For the CLI (rust) lane the timed unit is a subprocess; RSS uses
# RUSAGE_CHILDREN (max-of-dead-children on darwin, sum on linux — still a
# ceiling, never an underestimate of a single fat child).

_RSS = {}


def _child_maxrss_mb():
    """Peak RSS across finished children in MB (darwin unit = bytes)."""
    v = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    return v / (1024.0 * 1024.0) if sys.platform == "darwin" else v / 1024.0



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


def run_corpus(n_docs, per_doc=10, keep_root=None, engine_argv=None):
    """keep_root: a directory path to (re)use and leave behind for
    inspection; when None the corpus root is a cleaned-up tempdir.
    engine_argv: None -> in-process python wikicore; an argv prefix list
    -> subprocess lane (rust binary)."""
    import shutil
    import tempfile

    root = keep_root or tempfile.mkdtemp(prefix="llmwiki-perf-")
    try:
        if engine_argv is not None:
            return _run_corpus_cli(root, n_docs, per_doc, engine_argv)
        return _run_corpus(root, n_docs, per_doc)
    finally:
        if keep_root is None:
            shutil.rmtree(root, ignore_errors=True)


def _cli(engine_argv, args, cwd, input_text=None):
    """Run engine subprocess; returns parsed stdout JSON (raises on failure)."""
    import subprocess
    proc = subprocess.run(
        engine_argv + [str(a) for a in args], cwd=cwd, input=input_text,
        capture_output=True, text=True, timeout=300)
    if proc.returncode != 0:
        raise RuntimeError("%s %s exit=%d: %s"
                           % (engine_argv[0], args[0], proc.returncode,
                              proc.stderr.strip()[:300]))
    return json.loads(proc.stdout)


def _run_corpus_cli(root, n_docs, per_doc, engine_argv):
    """Rust-lane corpus run: identical op sequence via CLI subprocesses."""
    import subprocess
    import tempfile

    _cli(engine_argv, ["init", "--name", "perf"], cwd=root)

    def _ingest_stage():
        for n in range(n_docs):
            r = _cli(engine_argv,
                     ["ingest", "--text", "bench body %d" % n,
                      "--text-ref", "perf-doc-%06d" % n], cwd=root)
            cands = [gen_corpus._cand(r["source_id"], r["version"],
                                      gen_corpus.SUBJECT_POOL[
                                          (n * per_doc + k) % len(
                                              gen_corpus.SUBJECT_POOL)],
                                      gen_corpus.PREDICATES[
                                          (n + k) % len(
                                              gen_corpus.PREDICATES)],
                                      "v%d" % ((n + k) % 2))
                     for k in range(per_doc)]
            with tempfile.NamedTemporaryFile("w", suffix=".json",
                                             delete=False) as f:
                json.dump(cands, f)
                cpath = f.name
            with tempfile.NamedTemporaryFile("w", suffix=".json",
                                             delete=False) as f:
                json.dump(gen_corpus._report(r["source_id"], r["version"]), f)
                rpath = f.name
            _cli(engine_argv,
                 ["stage-candidates", "--file", cpath,
                  "--source-id", r["source_id"],
                  "--source-version", str(r["version"]),
                  "--report", rpath], cwd=root)
            os.unlink(cpath)
            os.unlink(rpath)

    def _staged_run_ids():
        base = os.path.join(root, ".llm-wiki", ".state", "staging")
        if not os.path.isdir(base):
            return []
        return sorted(d for d in os.listdir(base)
                      if d.startswith("run_")
                      and os.path.isdir(os.path.join(base, d)))

    ingest_s, _ = _t(_ingest_stage, rss_key="ingest")
    _RSS["ingest"] = round(_child_maxrss_mb(), 1)

    runs = _staged_run_ids()
    prepare_s = 0.0
    cls_paths = {}
    tmp_paths = []
    try:
        for rid in runs:
            def _prep(rid=rid):
                prep = _cli(engine_argv,
                            ["reconcile-prepare", "--run", rid], cwd=root)
                cls = [{"index": c["index"],
                        "relationship": "UNRELATED",
                        "target_claim_id": None, "valid_from": None,
                        "valid_to": None,
                        "authority": {"type": "authoritative_source"}}
                       for c in prep["comparisons"]]
                f = tempfile.NamedTemporaryFile("w", suffix=".json",
                                                delete=False)
                json.dump(cls, f)
                f.close()
                cls_paths[rid] = f.name
                tmp_paths.append(f.name)
                return prep
            dt, _ = _t(_prep, rss_key="prepare")
            prepare_s += dt
        _RSS["prepare"] = round(_child_maxrss_mb(), 1)

        def _apply():
            for rid in runs:
                _cli(engine_argv,
                     ["reconcile-apply", "--run", rid,
                      "--classifications", cls_paths[rid]], cwd=root)
        apply_s, _ = _t(_apply, rss_key="apply")
        _RSS["apply"] = round(_child_maxrss_mb(), 1)

        query_s, _ = _t(
            lambda: _cli(engine_argv,
                         ["query-prepare", "--question",
                          "who owns acme.svc00"], cwd=root),
            rss_key="query")
        _RSS["query"] = round(_child_maxrss_mb(), 1)
        graph_s, payload = _t(
            lambda: _cli(engine_argv, ["graph-data"], cwd=root),
            rss_key="graph")
        _RSS["graph"] = round(_child_maxrss_mb(), 1)
    finally:
        for p in tmp_paths:
            try:
                os.unlink(p)
            except OSError:
                pass

    claims_dir = os.path.join(root, ".llm-wiki", "claims")
    claims = sum(1 for f in os.listdir(claims_dir)
                 if f.startswith("claim_") and f.endswith(".json")) \
        if os.path.isdir(claims_dir) else 0
    out = {"docs": n_docs, "claims": claims,
           "ingest_s": round(ingest_s, 3), "ingest_rss_mb": _RSS["ingest"],
           "prepare_s": round(prepare_s, 3), "prepare_rss_mb": _RSS["prepare"],
           "apply_s": round(apply_s, 3), "apply_rss_mb": _RSS["apply"],
           "query_s": round(query_s, 3), "query_rss_mb": _RSS["query"],
           "graph_s": round(graph_s, 3), "graph_rss_mb": _RSS["graph"],
           "bytes_graph": len(json.dumps(payload))}
    _RSS.clear()
    return out


def _run_corpus(root, n_docs, per_doc):
    from wikicore import compile as wc_compile, graphdata, query  # noqa: E402
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
