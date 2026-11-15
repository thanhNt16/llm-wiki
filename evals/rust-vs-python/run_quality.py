#!/usr/bin/env python3
"""Quality lane runner for the rust-vs-python suite (P7 lane A).

    python3 run_quality.py --corpus corpus --lane {rust,python,baseline} \
        [--workdir DIR] [--out PATH]

Lanes:
    rust/python : init -> ingest -> stage-candidates -> reconcile
                  (deterministic classifications: CONTRADICTION for phase-1
                  planted conflicts, CORRECTION for phase-2 v2 docs) ->
                  question battery via query-prepare, context bytes via
                  context-pack budget_used, memory effectiveness via
                  dependent invalidation (context packs + one dep page).
    baseline    : keyword ranking over corpus files + PROJECT.md summary
                  (ported from evals/benchmark/run_benchmark.py); reads in
                  rank order until the first value match, bytes consumed =
                  sum of file bytes opened.

Determinism: no timestamps, ids, or absolute paths are written to the lane
JSON — only booleans, counts, and metric values.

Stdlib only, Python >= 3.9.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import _engine  # noqa: E402

TOP_K = 5
BASELINE_TOP_N = 3
SEMANTIC_METHOD = "token_jaccard"
SEMANTIC_THRESHOLD = 0.5
CONTEXT_METHOD = ("canonicalized query-prepare JSON payload bytes "
                  "(context-pack receipt budget_* is a non-stable token "
                  "estimate)")

BSTOPWORDS = set(
    "a an and are as at be but by for from has have how i in is it its of on or "
    "that the this to was we what when where which who why will with you our us "
    "do does did".split())
_TOKEN_RE = re.compile(r"[a-z0-9]+")


def _tokens(text):
    return set(t for t in _TOKEN_RE.findall(text.lower())
               if len(t) >= 3 and t not in BSTOPWORDS)


def semantic_score(expected, returned):
    """Token-overlap (Jaccard) between expected value string and the
    rendered returned value. Deterministic."""
    a = _tokens(str(expected))
    b = _tokens(str(returned))
    if not a:
        return 0.0
    inter = len(a & b)
    union = len(a | b)
    return inter / union if union else 0.0


def prf(tp, returned, expected):
    """Precision/recall/F1 given counts; expected==0 scores 0 unless
    returned==0 (perfect empty)."""
    if returned == 0 and expected == 0:
        return 1.0, 1.0, 1.0
    p = tp / returned if returned else 0.0
    r = tp / expected if expected else 0.0
    f = (2 * p * r / (p + r)) if (p + r) else 0.0
    return p, r, f


def micro_prf(per_q):
    tp = sum(q["tp"] for q in per_q)
    ret = sum(q["returned_n"] for q in per_q)
    exp = sum(q["expected_n"] for q in per_q)
    return prf(tp, ret, exp)


# ---------------------------------------------------------------- engine


def _cli(root, argv_prefix, *argv, ok_rcs=(0,)):
    cmd = argv_prefix + ["--root", root] + list(argv)
    p = subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True,
                       text=True, timeout=120)
    if p.returncode not in ok_rcs:
        raise RuntimeError("engine %s failed (rc=%d): %s %s"
                           % (" ".join(argv), p.returncode,
                              p.stdout.strip(), p.stderr.strip()))
    return json.loads(p.stdout)


def _classify_phase(prep, relationship):
    """Deterministic classifications for one reconcile run.

    Auto-verdict candidates are left for the engine. Every non-auto
    comparison gets `relationship` with target = first same-scope match
    (phase 1: CONTRADICTION for planted conflicts; phase 2: CORRECTION for
    v2 corrections). Anything else aborts — a silent misclassification
    would corrupt the metrics.
    """
    out = []
    for comp in prep["comparisons"]:
        if comp["auto"] is not None:
            continue
        target = None
        for m in comp["matches"]:
            if m["same_scope"]:
                target = m["claim_id"]
                break
        if not target:
            raise RuntimeError("no reconcile target for key %r index %d"
                               % (comp["key"], comp["index"]))
        out.append({"index": comp["index"], "relationship": relationship,
                    "target_claim_id": target})
    return out


def _ingest_doc(root, argv, path, cand_file, relationship, tmpdir):
    """ingest + stage + reconcile for one doc version. Returns source_id."""
    receipt = _cli(root, argv, "ingest", "--file", path)
    sid, ver = receipt["source_id"], receipt["version"]
    staged = _cli(root, argv, "stage-candidates", "--source-id", sid,
                  "--source-version", str(ver), "--file", cand_file)
    prep = _cli(root, argv, "reconcile-prepare", "--run", staged["run_id"])
    cls = _classify_phase(prep, relationship)
    cls_file = os.path.join(tmpdir, "cls-%s-v%d.json" % (sid[-8:], ver))
    with open(cls_file, "w") as f:
        json.dump(cls, f)
    _cli(root, argv, "reconcile-apply", "--run", staged["run_id"],
         "--classifications", cls_file)
    return sid


def _load_live_claims(wiki_dir):
    cdir = os.path.join(wiki_dir, "claims")
    out = []
    for name in sorted(os.listdir(cdir)):
        if not name.endswith(".json"):
            continue
        with open(os.path.join(cdir, name)) as f:
            c = json.load(f)
        if c.get("status") not in ("superseded", "rejected"):
            out.append(c)
    return out


def _resolve_claim(claims, subject, predicate, value):
    hits = [c for c in claims
            if c["subject"] == subject and c["predicate"] == predicate
            and str(c.get("value")) == str(value)]
    if len(hits) != 1:
        raise RuntimeError("claim %s/%s=%r resolved %d times"
                           % (subject, predicate, value, len(hits)))
    return hits[0]


# ---------------------------------------------------------------- scoring


def _score_wiki_question(out, fact, claim_map, source_by_file, k):
    """Score one query-prepare response against a label.

    claim_map: fact_id -> expected claim id(s) (v2 for corrections).
    Returns per-question record (deterministic fields only).
    """
    matches = (out.get("matches") or [])[:k]
    expected_ids = claim_map[fact["id"]]
    returned_ids = [m["claim_id"] for m in matches]
    tp = len(set(returned_ids) & set(expected_ids))

    top = matches[0] if matches else None
    corr = fact.get("correction")
    exact = stale = False
    if top is not None:
        top_val = str(top.get("value"))
        if corr:
            stale = bool(re.search(fact["expected_value"]["regex"],
                                   top_val))
            exact = bool(re.search(
                corr["expected_value_v2"]["regex"], top_val))
        else:
            accepted = [fact["expected_value"]["regex"]]
            if fact["conflict_case"]:
                accepted.append(fact["conflict_peer"]["value_regex"])
            exact = any(re.search(rx, top_val) for rx in accepted)

    best_sem = max((semantic_score(fact["expected_value"]["exact"],
                                   m.get("value")) for m in matches),
                   default=0.0)
    if corr:
        best_sem = max(best_sem, max(
            (semantic_score(corr["expected_value_v2"]["exact"],
                            m.get("value")) for m in matches), default=0.0))

    # citation: every matched claim's evidence locator resolves inside the
    # labeled source file for that claim.
    cite_ok = bool(matches)
    for m in matches:
        for ev in (m.get("evidence") or []):
            src = source_by_file.get(ev.get("source_id"))
            loc = (ev.get("locator") or {}).get("value") or ""
            if src is None or loc not in src:
                cite_ok = False
    res = {
        "fact_id": fact["id"],
        "returned_n": len(returned_ids),
        "expected_n": len(expected_ids),
        "tp": tp,
        "exact_match": exact,
        "semantic": round(best_sem, 6),
        "citation_ok": cite_ok,
        "conflict_surfaced": bool(out.get("conflicts")),
        "stale_answer": stale,
        "correct_current": exact and not stale,
    }
    return res


# ---------------------------------------------------------------- lanes


def run_wiki(engine_arg, corpus_dir, labels, tmpdir):
    argv = _engine.engine_argv(engine_arg)
    root = os.path.join(tmpdir, "proj")
    os.makedirs(root)
    _cli(root, argv, "init", "--name", "quality")

    source_by_file = {}   # source_id -> file text (current content)
    sid_of_doc = {}
    cand_dir = os.path.join(corpus_dir, "candidates")

    # phase 1: v1 ingest + stage + reconcile per doc, deterministic order
    for i in range(labels["docs"]):
        rel = "docs/doc-%03d.md" % i
        sid = _ingest_doc(root, argv, os.path.join(corpus_dir, rel),
                          os.path.join(cand_dir, "doc-%03d.jsonl" % i),
                          "CONTRADICTION", tmpdir)
        sid_of_doc[rel] = sid
        with open(os.path.join(corpus_dir, rel)) as f:
            source_by_file[sid] = f.read()

    # map each fact to its expected live claim id(s)
    claims = _load_live_claims(os.path.join(root, ".llm-wiki"))
    claim_map = {}
    for fact in labels["facts"]:
        c = _resolve_claim(claims, fact["subject"], fact["predicate"],
                           fact["expected_value"]["exact"])
        claim_map[fact["id"]] = [c["id"]]
        if fact["conflict_case"]:
            peer = _resolve_claim(claims, fact["subject"],
                                  fact["predicate"],
                                  fact["conflict_peer"]["value_exact"])
            claim_map[fact["id"]] = sorted([c["id"], peer["id"]])

    # phase A measurement: context packs + dep pages per correction case
    corrections = [f for f in labels["facts"] if f["correction_case"]]
    registered = []          # artifact relpaths registered as dependents
    for fact in corrections:
        out = _cli(root, argv, "context-pack", "--task", fact["question"],
                   "--budget", "8000")
        cid = fact_claim_id(claim_map, fact)
        if any(cl.split("@")[0] == cid
               for cl in out["receipt"]["claims"]):
            registered.append(out["pack_path"])
        # dep page citing the v1 claim
        page_text = ("---\ntype: concept\ntitle: %s\nsubject: %s\n"
                     "created: 2026-01-01T00:00:00Z\n---\n\n"
                     "Quality lane dependent page for %s.\n"
                     % (fact["id"], fact["subject"], fact["subject"]))
        pfile = os.path.join(tmpdir, "page-%s.md" % fact["id"])
        with open(pfile, "w") as f:
            f.write(page_text)
        artifact = "wiki/concepts/%s.md" % (
            fact["subject"].replace(".", "-"))
        _cli(root, argv, "write-page", "--file", pfile,
             "--artifact", artifact, "--deps", cid, "--allow-new")
        registered.append(artifact)

    # pre-correction answers (must reflect v1)
    pre_ok = {}
    for fact in corrections:
        out = _cli(root, argv, "query-prepare",
                   "--question", fact["question"])
        top = (out.get("matches") or [None])[0]
        pre_ok[fact["id"]] = bool(
            top is not None and re.search(
                fact["expected_value"]["regex"], str(top.get("value"))))

    # phase 2: correction docs — overwrite file, re-ingest v2, CORRECTION
    for fact in corrections:
        i = fact["doc_index"]
        rel = "docs/doc-%03d.md" % i
        shutil.copyfile(os.path.join(corpus_dir, "v2", "doc-%03d.md" % i),
                        os.path.join(corpus_dir, rel))
        sid = _ingest_doc(root, argv, os.path.join(corpus_dir, rel),
                          os.path.join(corpus_dir, "candidates_v2",
                                       "doc-%03d.jsonl" % i),
                          "CORRECTION", tmpdir)
        assert sid == sid_of_doc[rel], "correction re-ingest got new source"
        with open(os.path.join(corpus_dir, rel)) as f:
            source_by_file[sid] = f.read()

    # invalidation: share of registered dependents now stale per verify
    ver = _cli(root, argv, "verify", ok_rcs=(0, 3))
    stale_set = set(ver.get("stale") or [])
    invalidated = sum(1 for a in registered if a in stale_set)
    invalidation_rate = (invalidated / len(registered)
                         if registered else None)

    # expected claim ids now point at v2 for correction cases
    claims = _load_live_claims(os.path.join(root, ".llm-wiki"))
    for fact in corrections:
        c = _resolve_claim(claims, fact["subject"], fact["predicate"],
                           fact["correction"]["expected_value_v2"]["exact"])
        claim_map[fact["id"]] = [c["id"]]

    # question battery
    per_q = []
    for fact in labels["facts"]:
        out = _cli(root, argv, "query-prepare",
                   "--question", fact["question"])
        rec = _score_wiki_question(out, fact, claim_map, source_by_file,
                                   TOP_K)
        # context bytes: canonicalized query-prepare payload size. The
        # context-pack receipt's budget_used/estimated is a token estimate
        # and is not stable run to run (observed drift); the query payload
        # is byte-stable (ULIDs and score floats are fixed-width), so it
        # is the honest deterministic measure. Spec-declared additive keys
        # (rust-only `ranker`, `truncated_matches`, `deprecated_flag` —
        # design spec §6.1/§10.2, line 414) are excluded so the metric
        # measures context, not engine labeling.
        measured = {k: v for k, v in out.items()
                    if k not in ("ranker", "truncated_matches",
                                 "deprecated_flag")}
        rec["context_bytes"] = len(json.dumps(
            measured, sort_keys=True, separators=(",", ":")).encode("utf-8"))
        per_q.append(rec)

    memory = {
        "correction_cases": len(corrections),
        "pre_correction_correct": sum(1 for v in pre_ok.values() if v),
        "registered_dependents": len(registered),
        "invalidated_dependents": invalidated,
        "invalidation_rate": invalidation_rate,
        "stale_answer_rate": (
            sum(1 for q in per_q if q.get("stale_answer"))
            / len(corrections) if corrections else None),
    }
    return per_q, memory, {"engine": _engine.engine_name(engine_arg)}


def fact_claim_id(claim_map, fact):
    return claim_map[fact["id"]][0]


def run_baseline(corpus_dir, labels):
    """Keyword-ranking baseline (ported from run_benchmark._baseline_scenario
    ranking), extended with read-until-first-match byte accounting."""
    files = []
    order = ["PROJECT.md"] + ["docs/doc-%03d.md" % i
                              for i in range(labels["docs"])]
    for rel in order:
        with open(os.path.join(corpus_dir, rel)) as f:
            files.append((rel, f.read()))

    def ranked(question):
        q_toks = _tokens(question)
        scored = [(len(q_toks & _tokens(text)), rel, text)
                  for rel, text in files]
        scored.sort(key=lambda t: (-t[0], t[1]))
        return scored

    def scan(question, fact):
        """Read in rank order until first regex match (expected or
        obsolete); returns (matched_kind, bytes_read, files_read)."""
        want = re.compile(fact["expected_value"]["regex"])
        obs = None
        corr = fact.get("correction")
        if corr:
            obs = re.compile(fact["expected_value"]["regex"])
            want = re.compile(corr["expected_value_v2"]["regex"])
        elif fact["conflict_case"]:
            want = re.compile("(?:%s)|(?:%s)"
                              % (fact["expected_value"]["regex"],
                                 fact["conflict_peer"]["value_regex"]))
        bytes_read, read_files = 0, []
        for _score, rel, text in ranked(question):
            read_files.append(rel)
            bytes_read += len(text.encode("utf-8"))
            if obs and obs.search(text):
                return "obsolete", bytes_read, read_files
            if want.search(text):
                return "expected", bytes_read, read_files
        return "none", bytes_read, read_files

    # phase A (pre-correction): correction questions must resolve to v1
    # while the docs still carry v1 content.
    corrections = [f for f in labels["facts"] if f["correction_case"]]
    pre_ok = {}
    for fact in corrections:
        v1 = re.compile(fact["expected_value"]["regex"])
        pre_ok[fact["id"]] = any(
            v1.search(text) for _s, _r, text in
            ranked(fact["question"])[:BASELINE_TOP_N])

    # phase B: apply v2 corrections to docs (the hand summary is NOT
    # updated — that is the point of the memory-effectiveness measure).
    for fact in corrections:
        i = fact["doc_index"]
        rel = "docs/doc-%03d.md" % i
        shutil.copyfile(os.path.join(corpus_dir, "v2", "doc-%03d.md" % i),
                        os.path.join(corpus_dir, rel))
        for j, (r, _t) in enumerate(files):
            if r == rel:
                with open(os.path.join(corpus_dir, rel)) as f:
                    files[j] = (r, f.read())

    per_q = []
    for fact in labels["facts"]:
        kind, nbytes, read_files = scan(fact["question"], fact)
        top3 = read_files[:BASELINE_TOP_N]
        expected_files = [fact["source_file"]]
        if fact["conflict_case"]:
            expected_files.append(fact["conflict_peer"]["doc"])
        tp = len(set(top3) & set(expected_files))
        corr = fact.get("correction")
        rec = {
            "fact_id": fact["id"],
            "returned_n": len(top3),
            "expected_n": len(expected_files),
            "tp": tp,
            "exact_match": kind == "expected",
            "stale_answer": kind == "obsolete",
            "citation_ok": bool(set(top3) & set(expected_files)),
            "context_bytes": nbytes,
            "correct_current": kind == "expected",
        }
        # semantic score: expected-value token overlap against the text of
        # the first file that produced a match (or empty if no match)
        if kind != "none" and read_files:
            matched_rel = read_files[-1]
            matched_text = next(t for r, t in files if r == matched_rel)
            val = (corr["expected_value_v2"]["exact"]
                   if corr and kind == "expected"
                   else fact["expected_value"]["exact"])
            rec["semantic"] = round(semantic_score(val, matched_text), 6)
        else:
            rec["semantic"] = 0.0
        per_q.append(rec)

    memory = {
        "correction_cases": len(corrections),
        "pre_correction_correct": sum(1 for v in pre_ok.values() if v),
        "registered_dependents": 0,
        "invalidated_dependents": 0,
        "invalidation_rate": None,
        "stale_answer_rate": (
            sum(1 for q in per_q if q["stale_answer"])
            / len(corrections) if corrections else None),
    }
    return per_q, memory, {"engine": "baseline"}


# ---------------------------------------------------------------- report


def build_lane_json(lane, per_q, memory, meta):
    prec, rec, f1 = micro_prf(per_q)
    corr_bytes = [q["context_bytes"] for q in per_q]
    aggregates = {
        "k": TOP_K if lane != "baseline" else BASELINE_TOP_N,
        "questions": len(per_q),
        "precision": round(prec, 6),
        "recall": round(rec, 6),
        "f1": round(f1, 6),
        "exact_match_pct": round(
            sum(1 for q in per_q if q["exact_match"]) / len(per_q), 6),
        "mean_semantic": round(
            sum(q["semantic"] for q in per_q) / len(per_q), 6),
        "semantic_method": SEMANTIC_METHOD,
        "semantic_threshold": SEMANTIC_THRESHOLD,
        "citation_coverage_pct": round(
            sum(1 for q in per_q if q["citation_ok"]) / len(per_q), 6),
        "context_bytes_total": sum(corr_bytes),
        "context_bytes_mean": round(sum(corr_bytes) / len(corr_bytes), 3),
        "context_bytes_method": (
            CONTEXT_METHOD if lane != "baseline"
            else "bytes of files opened in rank order until first match"),
        "correct_current_pct": round(
            sum(1 for q in per_q if q.get("correct_current"))
            / len(per_q), 6),
    }
    return {
        "lane": lane,
        "engine": meta["engine"],
        "aggregates": aggregates,
        "memory": memory,
        "per_question": per_q,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--lane", required=True,
                    choices=("rust", "python", "baseline"))
    ap.add_argument("--workdir", default=None,
                    help="scratch dir (default: mkdtemp)")
    ap.add_argument("--out", default=None,
                    help="default: <corpus-parent>/quality-<lane>.json")
    args = ap.parse_args()

    corpus_dir = os.path.abspath(args.corpus)
    with open(os.path.join(corpus_dir, "labels.json")) as f:
        labels = json.load(f)

    out_path = args.out or os.path.join(
        os.path.dirname(corpus_dir), "quality-%s.json" % args.lane)

    tmp = args.workdir or tempfile.mkdtemp(prefix="llmwiki-quality-")
    try:
        # Work on a shadow copy of the corpus: correction cases overwrite
        # docs with v2 content mid-run; the input corpus stays pristine so
        # re-runs are byte-identical.
        work_corpus = os.path.join(tmp, "corpus")
        shutil.copytree(corpus_dir, work_corpus)
        if args.lane == "baseline":
            per_q, memory, meta = run_baseline(work_corpus, labels)
        else:
            per_q, memory, meta = run_wiki(args.lane, work_corpus, labels,
                                           tmp)
        lane_json = build_lane_json(args.lane, per_q, memory, meta)
    finally:
        if not args.workdir:
            shutil.rmtree(tmp, ignore_errors=True)

    with open(out_path, "w") as f:
        json.dump(lane_json, f, indent=2, sort_keys=True)
        f.write("\n")
    a = lane_json["aggregates"]
    print("%s: P=%.3f R=%.3f F1=%.3f exact=%.3f stale=%s -> %s"
          % (args.lane, a["precision"], a["recall"], a["f1"],
             a["exact_match_pct"], memory["stale_answer_rate"], out_path))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
