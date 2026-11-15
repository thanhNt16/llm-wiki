#!/usr/bin/env python3
"""run_perf.py — rust-vs-python performance lane (P7 lane B).

Times both engines (python wiki.py vs rust release binary) over an
identical deterministic corpus produced by gen_perf_corpus.py:

  ingestion    per-file wall (median/min/max over all files x reps) +
               full-corpus `ingest --dir` batch wall (cold), and a warm
               re-ingest of the same corpus (dedup path)
  compilation  stage-candidates (all sources) + reconcile-prepare +
               reconcile-apply, end-to-end, cold wiki, k=5
  query        p50/p95 over >=100 query-prepare calls on the seeded wiki,
               plus one query after poisoning the index watermark
               (rebuild/fallback cost), then a query after repair
  graph        graph-data wall + payload bytes, both engines; rust-only
               viz-serve time-to-first-frame (spawn, poll /, kill)
  memory       peak RSS for the ingest+compile pipeline via
               resource.getrusage(RUSAGE_CHILDREN) plus a /usr/bin/time -l
               single-invocation datapoint (batch ingest)
  footprint    release binary bytes (+ stripped variant), python engine
               bytes under skill/llm-wiki/scripts
  cold-start   `status` on a warm wiki, 20 reps median

Output: evals/rust-vs-python/perf.json (stable sorted structure; values
are measured medians/min/max with rep counts).

Stdlib only, Python >= 3.9. Writes only under --tmp (default /tmp).
"""
import argparse
import json
import os
import platform
import resource
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))  # evals/
sys.path.insert(0, HERE)

import _engine  # noqa: E402
import gen_perf_corpus as corpus  # noqa: E402

OUT_JSON = os.path.join(HERE, "perf.json")
DEFAULT_DOCS = 150
DEFAULT_REPS = 5
STATUS_REPS = 20


# ---------------------------------------------------------------- stats


def percentile(samples, p):
    """p in [0,100]; linear-interpolated percentile of samples."""
    if not samples:
        raise ValueError("percentile of empty sample set")
    xs = sorted(samples)
    if len(xs) == 1:
        return xs[0]
    rank = (p / 100.0) * (len(xs) - 1)
    lo = int(rank)
    frac = rank - lo
    hi = min(lo + 1, len(xs) - 1)
    return xs[lo] + frac * (xs[hi] - xs[lo])


def med_min_max(samples):
    """{'median','min','max','n'} over a sample list (seconds)."""
    if not samples:
        raise ValueError("empty sample set")
    return {"median": statistics.median(samples), "min": min(samples),
            "max": max(samples), "n": len(samples)}


def round_stats(d, nd=6):
    return {k: (round(v, nd) if isinstance(v, float) else v)
            for k, v in d.items()}


def speedup(py, rs):
    """rust-vs-python speedup factor; None when either side is n/a."""
    if not isinstance(py, (int, float)) or not isinstance(rs, (int, float)):
        return None
    if rs <= 0:
        return None
    return round(py / rs, 3)


# -------------------------------------------------------------- engine


def run_cmd(argv, cwd, env=None, timeout=600, check=True, stdin=None):
    """Run a command; return (proc, wall_seconds)."""
    t0 = time.perf_counter()
    proc = subprocess.run(argv, cwd=cwd, env=env, capture_output=True,
                          text=True, timeout=timeout, input=stdin)
    dt = time.perf_counter() - t0
    if check and proc.returncode != 0:
        raise RuntimeError(
            "%s exit=%d: %s" % (" ".join(str(a) for a in argv),
                                proc.returncode,
                                (proc.stderr or proc.stdout).strip()[:400]))
    return proc, dt


def engine_cli(eng_argv, args, cwd, timeout=600):
    """Engine subprocess expecting one JSON object on stdout."""
    proc, dt = run_cmd(eng_argv + [str(a) for a in args], cwd, timeout=timeout)
    return json.loads(proc.stdout), dt


def init_wiki(eng_argv, root):
    os.makedirs(root, exist_ok=True)
    engine_cli(eng_argv, ["init", "--name", "perf"], cwd=root)


def stage_source(eng_argv, root, sid, ver, doc_i, tmpdir):
    """Write + stage candidate/report files for one source; return run_id."""
    cpath = os.path.join(tmpdir, "cands_%s.json" % sid)
    rpath = os.path.join(tmpdir, "rep_%s.json" % sid)
    with open(cpath, "w") as f:
        json.dump(corpus.candidates_for(sid, ver, doc_i), f)
    with open(rpath, "w") as f:
        json.dump(corpus.report_for(sid, ver), f)
    out, _ = engine_cli(eng_argv,
                        ["stage-candidates", "--file", cpath,
                         "--source-id", sid, "--source-version", str(ver),
                         "--report", rpath],
                        cwd=root)
    os.unlink(cpath)
    os.unlink(rpath)
    return out.get("run_id") or out.get("run")


def compile_all(eng_argv, root, run_ids, tmpdir):
    """reconcile-prepare + reconcile-apply over every staged run.

    Returns (prepare_s, apply_s). All comparisons classified UNRELATED so
    every candidate becomes a fresh claim (bench.py precedent).
    """
    prepare_s = 0.0
    cls_paths = {}
    for rid in run_ids:
        prep, dt = engine_cli(eng_argv, ["reconcile-prepare", "--run", rid],
                              cwd=root)
        prepare_s += dt
        cls = [{"index": c["index"], "relationship": "UNRELATED",
                "target_claim_id": None, "valid_from": None,
                "valid_to": None,
                "authority": {"type": "authoritative_source"}}
               for c in prep["comparisons"]]
        cpath = os.path.join(tmpdir, "cls_%s.json" % rid)
        with open(cpath, "w") as f:
            json.dump(cls, f)
        cls_paths[rid] = cpath
    apply_s = 0.0
    for rid in run_ids:
        _, dt = engine_cli(eng_argv,
                           ["reconcile-apply", "--run", rid,
                            "--classifications", cls_paths[rid]],
                           cwd=root)
        apply_s += dt
        os.unlink(cls_paths[rid])
    return prepare_s, apply_s


def seed_wiki(eng_argv, root, corpus_dir, files, tmpdir, time_ingest=False):
    """init -> batch ingest -> stage -> prepare -> apply.

    Returns dict of timings + source map. time_ingest=True also returns
    per-file ingest samples (per-file loop replaces the batch ingest).
    """
    init_wiki(eng_argv, root)
    sources = {}
    per_file = []
    if time_ingest:
        t0 = time.perf_counter()
        for i, path in enumerate(files):
            r, dt = engine_cli(eng_argv, ["ingest", "--file", path], cwd=root)
            per_file.append(dt)
            sources[i] = (r["source_id"], r["version"])
        batch_s = time.perf_counter() - t0
    else:
        receipt, batch_s = engine_cli(eng_argv,
                                      ["ingest", "--dir", corpus_dir],
                                      cwd=root)
        for item in receipt["items"]:
            if item["status"] in ("ingested", "deduplicated",
                                  "updated_normalized"):
                i = int(item["path"][4:8])  # svc-NNNN.md
                sources[i] = (item["source_id"], item["version"])
    stage_s = 0.0
    run_ids = []
    for i in sorted(sources):
        sid, ver = sources[i]
        t0 = time.perf_counter()
        run_ids.append(stage_source(eng_argv, root, sid, ver, i, tmpdir))
        stage_s += time.perf_counter() - t0
    prepare_s, apply_s = compile_all(eng_argv, root, run_ids, tmpdir)
    return {"sources": sources, "per_file": per_file,
            "batch_s": batch_s, "stage_s": stage_s,
            "prepare_s": prepare_s, "apply_s": apply_s,
            "compile_s": stage_s + prepare_s + apply_s}


def mini_claim_commit(eng_argv, root, tag, tmpbase):
    """One claim-writing commit: ingest a fresh text doc, stage one
    candidate, prepare, apply. Returns total wall seconds."""
    t0 = time.perf_counter()
    r, _ = engine_cli(eng_argv,
                      ["ingest", "--text", "probe body %s" % tag,
                       "--text-ref", "probe-%s" % tag],
                      cwd=root)
    sid, ver = r["source_id"], r["version"]
    with tempfile.TemporaryDirectory(dir=tmpbase) as td:
        rid = stage_source(eng_argv, root, sid, ver, 0, td)
        prep, _ = engine_cli(eng_argv, ["reconcile-prepare", "--run", rid],
                             cwd=root)
        cls = [{"index": c["index"], "relationship": "UNRELATED",
                "target_claim_id": None, "valid_from": None,
                "valid_to": None,
                "authority": {"type": "authoritative_source"}}
               for c in prep["comparisons"]]
        cpath = os.path.join(td, "cls.json")
        with open(cpath, "w") as f:
            json.dump(cls, f)
        engine_cli(eng_argv, ["reconcile-apply", "--run", rid,
                              "--classifications", cpath], cwd=root)
    return time.perf_counter() - t0


# ---------------------------------------------------------- index poison


CLAIMS_INDEX_REL = os.path.join(".llm-wiki", ".state", "claims-index.json")
SQLITE_INDEX_REL = os.path.join(".llm-wiki", ".state", "index.sqlite")


def _poison_claims_json(path):
    with open(path) as f:
        idx = json.load(f)
    idx["revision"] = 0
    with open(path, "w") as f:
        json.dump(idx, f)


def _poison_sqlite(path):
    import sqlite3
    con = sqlite3.connect(path)
    try:
        con.execute("INSERT OR REPLACE INTO meta (k, v) VALUES "
                    "('built_at_revision', '0')")
        con.commit()
    finally:
        con.close()


def poison_index(root, engine):
    """Set the engine's query-index watermark stale.

    python: .state/claims-index.json is the query index — set revision=0.
    rust:   query lane selection reads .state/index.sqlite
            (meta.built_at_revision); the legacy claims-index.json is
            poisoned too so every cached watermark reports stale.
    Returns list of (kind, path) poisoned."""
    hit = []
    cj = os.path.join(root, CLAIMS_INDEX_REL)
    sq = os.path.join(root, SQLITE_INDEX_REL)
    if engine == "rust" and os.path.isfile(sq):
        _poison_sqlite(sq)
        hit.append(("rust-sqlite", sq))
        if os.path.isfile(cj):
            _poison_claims_json(cj)
            hit.append(("claims-index-json", cj))
    elif os.path.isfile(cj):
        _poison_claims_json(cj)
        hit.append(("claims-index-json", cj))
    elif os.path.isfile(sq):
        _poison_sqlite(sq)
        hit.append(("rust-sqlite", sq))
    return hit


def watermarks(root):
    """{file_rel: watermark} for every index file present."""
    out = {}
    cj = os.path.join(root, CLAIMS_INDEX_REL)
    sq = os.path.join(root, SQLITE_INDEX_REL)
    if os.path.isfile(cj):
        try:
            with open(cj) as f:
                out["claims-index.json"] = json.load(f).get("revision")
        except (OSError, ValueError):
            out["claims-index.json"] = None
    if os.path.isfile(sq):
        import sqlite3
        con = sqlite3.connect(sq)
        try:
            row = con.execute(
                "SELECT v FROM meta WHERE k='built_at_revision'").fetchone()
            out["index.sqlite"] = int(row[0]) if row else None
        except Exception:
            out["index.sqlite"] = None
        finally:
            con.close()
    return out


def child_maxrss_mb():
    v = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    return v / (1024.0 * 1024.0) if sys.platform == "darwin" else v / 1024.0


def time_to_first_frame(eng_argv, root, port=5417):
    """Spawn viz-serve, poll / until a 200 with a body; return seconds."""
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        eng_argv + ["viz-serve", "--port", str(port), "--no-open",
                    "--root", root],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    first = None
    try:
        deadline = t0 + 30
        while time.perf_counter() < deadline:
            if proc.poll() is not None:
                break
            try:
                with urllib.request.urlopen(
                        "http://127.0.0.1:%d/" % port, timeout=1) as resp:
                    body = resp.read(4096)
                    if resp.status == 200 and body:
                        first = time.perf_counter() - t0
                        break
            except Exception:
                time.sleep(0.02)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    return first


def time_l_maxrss(argv, cwd):
    """Run argv under /usr/bin/time -l; return (maxrss_bytes, wall_s)."""
    t0 = time.perf_counter()
    proc = subprocess.run(["/usr/bin/time", "-l"] + argv, cwd=cwd,
                          capture_output=True, text=True, timeout=900)
    dt = time.perf_counter() - t0
    if proc.returncode != 0:
        raise RuntimeError("time -l run failed: %s" % proc.stderr[:300])
    peak = None
    for line in proc.stderr.splitlines():
        if "maximum resident set size" in line:
            peak = int(line.split()[0])
    return peak, dt


# ----------------------------------------------------------------- main


def measure_engine(name, eng_argv, corpus_dir, files, docs, reps, tmpbase,
                   n_queries):
    out = {"name": name}
    per_file_samples = []
    batch_cold = []
    compile_total = {"stage_s": [], "prepare_s": [], "apply_s": [],
                     "total_s": []}
    keep_root = os.path.join(tmpbase, "wiki-%s" % name)
    shutil.rmtree(keep_root, ignore_errors=True)

    for rep in range(reps):
        root = keep_root if rep == reps - 1 else os.path.join(
            tmpbase, "wiki-%s-rep%d" % (name, rep))
        shutil.rmtree(root, ignore_errors=True)
        with tempfile.TemporaryDirectory(dir=tmpbase) as tmpdir:
            res = seed_wiki(eng_argv, root, corpus_dir, files, tmpdir,
                            time_ingest=True)
        per_file_samples.extend(res["per_file"])
        for k in compile_total:
            src = {"stage_s": "stage_s", "prepare_s": "prepare_s",
                   "apply_s": "apply_s", "total_s": "compile_s"}[k]
            compile_total[k].append(res[src])
    pipeline_rss_after = child_maxrss_mb()

    # Cold batch ingest (separate fresh wikis, k reps).
    for rep in range(reps):
        root = os.path.join(tmpbase, "wiki-%s-batch%d" % (name, rep))
        shutil.rmtree(root, ignore_errors=True)
        init_wiki(eng_argv, root)
        _, dt = engine_cli(eng_argv, ["ingest", "--dir", corpus_dir],
                           cwd=root)
        batch_cold.append(dt)
        shutil.rmtree(root, ignore_errors=True)

    out["ingest"] = {
        "per_file_s": round_stats(med_min_max(per_file_samples)),
        "batch_cold_s": round_stats(med_min_max(batch_cold)),
    }
    out["compile"] = {
        "stage_candidates_s": round_stats(med_min_max(compile_total["stage_s"])),
        "reconcile_prepare_s": round_stats(med_min_max(compile_total["prepare_s"])),
        "reconcile_apply_s": round_stats(med_min_max(compile_total["apply_s"])),
        "total_s": round_stats(med_min_max(compile_total["total_s"])),
    }

    root = keep_root

    # Warm re-ingest (dedup path) on the seeded wiki.
    warm = []
    for _ in range(reps):
        _, dt = engine_cli(eng_argv, ["ingest", "--dir", corpus_dir],
                           cwd=root)
        warm.append(dt)
    out["ingest"]["warm_rededup_s"] = round_stats(med_min_max(warm))

    # Query latency over >=100 deterministic questions.
    qs = corpus.questions(docs)
    if len(qs) < n_queries:
        qs = (qs * (n_queries // len(qs) + 1))[:n_queries]
    else:
        qs = qs[:n_queries]
    lat = []
    for q in qs:
        _, dt = engine_cli(eng_argv, ["query-prepare", "--question", q],
                           cwd=root)
        lat.append(dt)
    out["query"] = {
        "n": len(lat),
        "p50_s": round(percentile(lat, 50), 6),
        "p95_s": round(percentile(lat, 95), 6),
        "min_s": round(min(lat), 6), "max_s": round(max(lat), 6),
        "median_s": round(statistics.median(lat), 6),
    }

    # Steady-state commit baseline (before poisoning): the index repair
    # lands on the next claim-writing commit, not on a read, so the
    # rebuild cost is (poisoned mini-commit) - (healthy mini-commit).
    steady_commit = mini_claim_commit(eng_argv, root, "steady", tmpbase)

    # Poisoned index: stale watermark -> one query (fallback lane),
    # then a second query plus a claim-writing repair commit.
    hit = poison_index(root, name)
    wm_before = watermarks(root)
    _, poisoned_dt = engine_cli(
        eng_argv, ["query-prepare", "--question", qs[0]], cwd=root)
    wm_after = watermarks(root)
    _, recovered_dt = engine_cli(
        eng_argv, ["query-prepare", "--question", qs[1]], cwd=root)
    repair_dt = mini_claim_commit(eng_argv, root, "repair", tmpbase)
    wm_repaired = watermarks(root)
    out["query"]["poisoned_index"] = {
        "poisoned": [{"kind": k, "path": p} for k, p in hit],
        "watermarks_before_query": wm_before,
        "watermarks_after_query": wm_after,
        "watermarks_after_repair": wm_repaired,
        "first_query_s": round(poisoned_dt, 6),
        "second_query_s": round(recovered_dt, 6),
        "rebuilt_on_query": wm_after != wm_before,
        "repair_commit_s": round(repair_dt, 6),
        "repair_commit_steady_s": round(steady_commit, 6),
        "rebuild_cost_s": round(repair_dt - steady_commit, 6),
        "repair_method": ("wall of ingest+stage-candidates+"
                          "reconcile-prepare+reconcile-apply for one "
                          "fresh source, poisoned vs healthy wiki"),
    }

    # graph-data payload build.
    payload_s = []
    payload_bytes = None
    for _ in range(reps):
        t0 = time.perf_counter()
        proc = subprocess.run(eng_argv + ["--root", root, "graph-data"],
                              capture_output=True, text=True, timeout=300)
        if proc.returncode != 0:
            raise RuntimeError("graph-data failed: %s" % proc.stderr[:300])
        dt = time.perf_counter() - t0
        payload_s.append(dt)
        payload_bytes = len(proc.stdout.encode("utf-8"))
    out["graph"] = {
        "graph_data_s": round_stats(med_min_max(payload_s)),
        "payload_bytes": payload_bytes,
    }

    # viz-serve time-to-first-frame (rust only; caller decides n/a).
    if name == "rust":
        ttf = []
        for r in range(reps):
            v = time_to_first_frame(eng_argv, root, port=5417 + r * 3)
            if v is not None:
                ttf.append(v)
        out["graph"]["viz_serve_ttff_s"] = (
            round_stats(med_min_max(ttf)) if ttf else
            {"n": 0, "reason": "no successful 200 response within 30s"})
    else:
        out["graph"]["viz_serve_ttff_s"] = {
            "n": 0,
            "reason": "n/a: python engine is the dev-reference CLI; the "
                      "install target is the rust binary (viz-serve is "
                      "measured on the shipped engine only)"}

    # Peak RSS for the ingest+compile pipeline (getrusage children) +
    # /usr/bin/time -l datapoint on the fattest single invocation.
    out["peak_rss"] = {
        "pipeline_max_rss_mb": round(pipeline_rss_after, 1),
        "pipeline_method": ("resource.getrusage(RUSAGE_CHILDREN) peak "
                            "across all engine subprocesses during the "
                            "k=5 init+per-file-ingest+stage+prepare+apply "
                            "pipeline"),
    }
    troot = os.path.join(tmpbase, "wiki-%s-timerss" % name)
    shutil.rmtree(troot, ignore_errors=True)
    init_wiki(eng_argv, troot)
    peak, w = time_l_maxrss(eng_argv + ["ingest", "--dir", corpus_dir], troot)
    out["peak_rss"]["ingest_dir_time_l"] = {
        "max_rss_mb": round(peak / (1024.0 * 1024.0), 1) if peak else None,
        "wall_s": round(w, 3), "method": "/usr/bin/time -l"}
    shutil.rmtree(troot, ignore_errors=True)

    # Cold start: status on the warm wiki.
    starts = []
    for _ in range(STATUS_REPS):
        _, dt = engine_cli(eng_argv, ["status"], cwd=root)
        starts.append(dt)
    out["cold_start"] = {"status_s": round_stats(med_min_max(starts)),
                         "reps": STATUS_REPS}
    return out


def footprint(tmpbase):
    """Binary size + stripped variant + python engine byte count."""
    rust_bin = (os.environ.get("LLM_WIKI_BINARY") or _engine.RUST_BINARY)
    size = os.path.getsize(rust_bin)
    stripped_bytes = None
    strip = shutil.which("strip")
    if strip:
        dst = os.path.join(tmpbase, "llm-wiki.stripped")
        proc = subprocess.run([strip, "-o", dst, rust_bin],
                              capture_output=True, text=True)
        if proc.returncode == 0 and os.path.isfile(dst):
            stripped_bytes = os.path.getsize(dst)
    py_dir = os.path.dirname(_engine.WIKI_PY)
    py_bytes = 0
    py_files = 0
    for dirpath, _dirs, fnames in os.walk(py_dir):
        if "__pycache__" in dirpath:
            continue
        for fn in fnames:
            if fn.endswith((".py", ".sh")) or fn == "wiki.py":
                py_bytes += os.path.getsize(os.path.join(dirpath, fn))
                py_files += 1
    return {"rust_binary_bytes": size,
            "rust_binary_stripped_bytes": stripped_bytes,
            "python_engine_bytes": py_bytes,
            "python_engine_files": py_files,
            "python_engine_dir": py_dir,
            "note": ("python engine also requires a Python >=3.9 runtime "
                     "on PATH; the rust binary is self-contained")}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--docs", type=int, default=DEFAULT_DOCS)
    ap.add_argument("--reps", type=int, default=DEFAULT_REPS)
    ap.add_argument("--queries", type=int, default=120)
    ap.add_argument("--tmp", default=None,
                    help="scratch dir root (default: mktemp under /tmp)")
    ap.add_argument("--corpus", default=None,
                    help="reuse a generated corpus dir")
    ap.add_argument("--out", default=OUT_JSON)
    args = ap.parse_args()

    tmpbase = args.tmp or tempfile.mkdtemp(prefix="rvsperf-")
    os.makedirs(tmpbase, exist_ok=True)
    corpus_dir = args.corpus or os.path.join(tmpbase, "corpus")
    if not os.path.isdir(corpus_dir):
        docs = corpus.write_corpus(corpus_dir, args.docs)
    else:
        docs = corpus.gen_docs(args.docs)
    files = sorted(os.path.join(corpus_dir, n) for n, _ in docs)

    engines = {}
    for name in ("python", "rust"):
        engines[name] = _engine.engine_argv(name)

    results = {"meta": {
        "lane": "rust-vs-python/B",
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": sys.version.split()[0],
        "docs": args.docs, "reps": args.reps, "queries": args.queries,
        "corpus_dir": corpus_dir,
        "engines": {n: " ".join(e) for n, e in engines.items()},
    }, "engines": {}}

    for name in ("python", "rust"):
        results["engines"][name] = measure_engine(
            name, engines[name], corpus_dir, files, args.docs,
            args.reps, tmpbase, args.queries)

    results["footprint"] = footprint(tmpbase)

    py = results["engines"]["python"]
    rs = results["engines"]["rust"]
    results["headline_speedups"] = {
        "ingest_per_file": speedup(py["ingest"]["per_file_s"]["median"],
                                   rs["ingest"]["per_file_s"]["median"]),
        "ingest_batch_cold": speedup(py["ingest"]["batch_cold_s"]["median"],
                                     rs["ingest"]["batch_cold_s"]["median"]),
        "ingest_warm_dedup": speedup(py["ingest"]["warm_rededup_s"]["median"],
                                     rs["ingest"]["warm_rededup_s"]["median"]),
        "compile_total": speedup(py["compile"]["total_s"]["median"],
                                 rs["compile"]["total_s"]["median"]),
        "query_p50": speedup(py["query"]["p50_s"], rs["query"]["p50_s"]),
        "query_p95": speedup(py["query"]["p95_s"], rs["query"]["p95_s"]),
        "graph_data": speedup(py["graph"]["graph_data_s"]["median"],
                              rs["graph"]["graph_data_s"]["median"]),
        "cold_start_status": speedup(py["cold_start"]["status_s"]["median"],
                                     rs["cold_start"]["status_s"]["median"]),
    }

    tmp_out = args.out + ".tmp"
    with open(tmp_out, "w") as f:
        json.dump(results, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp_out, args.out)
    print(json.dumps(results["headline_speedups"], indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
