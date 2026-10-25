#!/usr/bin/env python3
"""Benchmark harness (PRD §60): wiki lane vs hand-maintained-file baseline.

Two lanes answer identical questions over evals/golden-corpus:

- wiki lane: builds a real .llm-wiki through wiki.py (init -> per-file ingest
  -> per-source stage-candidates -> reconcile-prepare/apply -> write-page
  (bench gold pages + planted doctor fixtures) -> build-pages -> verify ->
  doctor -> query-prepare), then scores matches/conflicts/gaps.
- baseline lane: pure-Python keyword search over the corpus files plus
  bench/PROJECT.md (the "manual summary" a project would realistically keep).
  Tokenizes the question, ranks files by matched-token count, reads the top
  3 files fully. File-granularity traceability only; it cannot signal gaps.

Deterministic: same inputs -> identical results.json. No network, no models.
Usage: `python3 evals/benchmark/run_benchmark.py` (prints table, exits 1 on
wiki-lane failure) or `run() -> dict` for the deterministic gate.
"""
import collections
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
WIKI_PY = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")
SCRIPTS = os.path.join(REPO, "skill", "llm-wiki", "scripts")
sys.path.insert(0, SCRIPTS)
from wikicore.synthesize import _split_front as _page_front

CORPUS = os.path.join(REPO, "evals", "golden-corpus")
BENCH = os.path.join(CORPUS, "bench")
RESULTS_PATH = os.path.join(HERE, "results.json")

# Deterministic ingest order: each source's candidates are staged+applied
# before the next ingest, so reconcile verdicts (supersede/corroborate/
# contradict) see exactly the claims listed below.
INGEST_ORDER = [
    "docs/adr-001-postgres.md",
    "docs/adr-007-retries.md",
    "docs/adr-019-attribution.md",
    "notes/memo-2026-09-01-window-change.md",
    "docs/spec-attribution-window.pdf",
    "notes/meeting-2026-09-10.md",
    "config/analytics.prod.json",
    "config/analytics.sandbox.json",
    "services/analytics/attribution.py",
    "data/config-matrix.csv",
    "notes/failed-redis-pubsub.md",
    "transcripts/session-2026-09-12.md",
    "README.md",
]

# Files the baseline reads: corpus minus eval artifacts, plus the manual
# project summary. questions.txt is an eval artifact, not project knowledge.
BASELINE_FILES = ["bench/PROJECT.md"] + INGEST_ORDER

# reconcile-prepare "comparisons" key -> (relationship, target pick rule)
# for candidates the auto-verdict cannot classify. Pick rules:
#   "first" -> matches[0]; "same_scope_value" -> first match with same scope
#   and same current_value as the candidate.
RECONCILE_OVERRIDES = {
    ("memo-2026-09-01-window-change", "attribution.window/click_lookback_days"):
        ("POLICY_CHANGE", "first"),
    ("meeting-2026-09-10", "payment.gateway/retries"):
        ("CONTRADICTION", "first"),
    ("analytics.prod", "payment.gateway/retries"):
        ("CORROBORATION", "same_scope_value"),
    ("analytics.sandbox", "attribution.window/click_lookback_days"):
        ("SCOPE_DIFFERENCE", "first"),
    ("analytics.sandbox", "payment.gateway/retries"):
        ("SCOPE_DIFFERENCE", "first"),
    ("attribution", "attribution.window/click_lookback_days"):
        ("CORROBORATION", "same_scope_value"),
    ("config-matrix", "attribution.window/click_lookback_days"):
        ("CORROBORATION", "same_scope_value"),
    ("config-matrix", "payment.gateway/retries"):
        ("CORROBORATION", "same_scope_value"),
    ("config-matrix", "svc.order.db/engine"):
        ("SCOPE_DIFFERENCE", "first"),
}
_TARGETED = ("CORRECTION", "POLICY_CHANGE", "CONTRADICTION",
             "CORROBORATION", "DUPLICATE")

EXPECTED_DOCTOR_WARNS = {
    "orphan_page": 1, "dead_end_page": 1, "walled_garden": 1,
    "near_duplicate_subject": 1, "stale_page_ttl": 1,
}
# Staging dirs accumulate per reconcile run and are expected, not asserted
# exactly (count = number of staged candidate runs); recorded in warn_counts.

BSTOPWORDS = set(
    "a an and are as at be but by for from has have how i in is it its of on or "
    "that the this to was we what when where which who why will with you our us "
    "do does did".split())
_TOKEN_RE = re.compile(r"[a-z0-9]+")
_PAGE_LINK_RE = re.compile(r"\[\[wiki:([a-z]+)/([a-z0-9][a-z0-9\-]{0,58}[a-z0-9])\]\]")
_TOP3 = 3

def _cli(root, *argv, ok_rcs=(0,), stdin_file=None):
    """Run wiki.py --root <root> <argv>; return parsed stdout JSON.

    ok_rcs tolerates semantic exits (verify/doctor use 3 for "findings"),
    which the caller scores instead of treating as crashes."""
    cmd = [sys.executable, WIKI_PY, "--root", root] + list(argv)
    stdin = open(stdin_file, "rb") if stdin_file else open(os.devnull, "rb")
    try:
        p = subprocess.run(cmd, stdin=stdin, capture_output=True, text=True,
                           timeout=120)
    finally:
        stdin.close()
    if p.returncode not in ok_rcs:
        raise RuntimeError("wiki.py %s failed (rc=%d): %s %s"
                           % (" ".join(argv), p.returncode,
                              p.stdout.strip(), p.stderr.strip()))
    return json.loads(p.stdout), len(p.stdout.encode("utf-8"))




def _classify(prep, stem):
    """Deterministic classifications for one reconcile run.

    auto-verdict candidates are left for the engine; candidates with no auto
    verdict get their planted relationship from RECONCILE_OVERRIDES.
    """
    out = []
    for comp in prep["comparisons"]:
        if comp["auto"] is not None:
            continue
        key = (stem, comp["key"])
        if key not in RECONCILE_OVERRIDES:
            raise RuntimeError("no reconcile rule for %s index %d"
                               % (key, comp["index"]))
        rel, pick = RECONCILE_OVERRIDES[key]
        cls = {"index": comp["index"], "relationship": rel}
        if rel in _TARGETED:
            target = None
            for m in comp["matches"]:
                if pick == "first" or (
                        pick == "same_scope_value" and m["same_scope"]
                        and m["current_value"] == comp["candidate_value"]):
                    target = m["claim_id"]
                    break
            if not target:
                raise RuntimeError("no reconcile target for %s index %d"
                                   % (key, comp["index"]))
            cls["target_claim_id"] = target
        if rel == "POLICY_CHANGE" and comp.get("valid_from"):
            cls["valid_from"] = comp["valid_from"]
        out.append(cls)
    return out


def _load_claims(wiki_dir):
    """subject/predicate/value/scope -> claim_id for live claims (read-only)."""
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


def _resolve_dep(claims, spec):
    """'subject|predicate|value|scopevalue' -> claim id (exactly one match)."""
    parts = spec.split("|")
    subject, predicate, value = parts[0], parts[1], parts[2]
    scopeval = parts[3] if len(parts) > 3 else ""
    hits = []
    for c in claims:
        if c["subject"] != subject or c["predicate"] != predicate:
            continue
        if str(c.get("value")) != value:
            continue
        scope = c.get("scope") or {}
        if scopeval:
            if len(scope) != 1 or next(iter(scope.values())) != scopeval:
                continue
        elif scope:
            continue
        hits.append(c["id"])
    if len(hits) != 1:
        raise RuntimeError("bench_dep %r resolved to %d claims" % (spec, len(hits)))
    return hits[0]


def _strip_see_also(text):
    """Page text without the '## See also' section (phase-A page write)."""
    return re.split(r"\n## See also\s*\n", text)[0].rstrip() + "\n"


def _page_deps(text):
    return sorted("wiki/%s/%s.md" % m for m in _PAGE_LINK_RE.findall(text))


def _write_bench_pages(root, tmpdir, claims):
    """Write every bench page through write-page (three passes).

    Phase A: claims-only deps, no [[wiki:]] links — the page graph has
    cycles, so no write order satisfies link-at-write-time. Phase B: full
    fixture body (See also links); pages whose link targets sort later in
    the fixture order fail that write and are retried in the fixup pass,
    once every page exists on disk.
    """
    pages_dir = os.path.join(BENCH, "pages")
    fixtures = []
    for kind in sorted(os.listdir(pages_dir)):
        kdir = os.path.join(pages_dir, kind)
        if not os.path.isdir(kdir):
            continue
        for name in sorted(os.listdir(kdir)):
            if name.endswith(".md"):
                fixtures.append((kind, name, os.path.join(kdir, name)))

    def staged_text(kind, name, path, body):
        fpath = os.path.join(tmpdir, "page-%s-%s" % (kind, name))
        with open(fpath, "w", encoding="utf-8") as f:
            f.write(body)
        return fpath

    def deps_for(text):
        front = _page_front(text)[0]
        return [_resolve_dep(claims, s) for s in front.get("bench_deps") or []]

    deferred = []
    for kind, name, path in fixtures:
        artifact = "wiki/%s/%s" % (kind, name)
        with open(path, encoding="utf-8") as f:
            text = f.read()
        # phase A: claims-only deps, no page links. --allow-new: the bench
        # deliberately plants near-duplicate-titled fixtures (the walled
        # garden island) that the near-dup guard would otherwise refuse at
        # exactly the 0.60 threshold.
        _cli(root, "write-page",
             "--file", staged_text(kind, "a-" + name, path,
                                   _strip_see_also(text)),
             "--artifact", artifact, "--deps", ",".join(deps_for(text)),
             "--allow-new")
        deferred.append((artifact, path, text))

    # phase B + fixup: full bodies with See also links; first attempt may
    # race a not-yet-written link target, retry once everything exists.
    todo = list(deferred)
    for _ in range(2):
        retry = []
        for artifact, path, text in todo:
            deps = deps_for(text) + _page_deps(text)
            try:
                _cli(root, "write-page",
                     "--file", staged_text(artifact.split("/")[1], "b-"
                                           + os.path.basename(path), path,
                                           text),
                     "--artifact", artifact, "--deps", ",".join(deps))
            except RuntimeError as e:
                if "does not exist on disk" not in str(e):
                    raise
                retry.append((artifact, path, text))
        todo = retry
    if todo:
        raise RuntimeError("unresolvable page links: %s"
                           % [a for a, _, _ in todo])

    # phase C: upstream staleness propagation marks a page stale when a page
    # it links to changes body (df60042). The phase-B writes above re-body
    # every page, so earlier-written linkers go stale. Re-write each stale
    # page with its identical fixture text until the set clears; bounded by
    # page count since each write clears at least its own flag.
    by_artifact = {a: (p, t) for a, p, t in deferred}
    for _ in range(len(deferred) + 2):
        ver, _ = _cli(root, "verify", ok_rcs=(0, 3))
        stale = [s for s in ver["stale"] if s in by_artifact]
        if not stale:
            break
        for artifact in stale:
            path, text = by_artifact[artifact]
            deps = deps_for(text) + _page_deps(text)
            _cli(root, "write-page",
                 "--file", staged_text(artifact.split("/")[1], "c-"
                                       + os.path.basename(path), path,
                                       text),
                 "--artifact", artifact, "--deps", ",".join(deps))
    else:
        raise RuntimeError("stale-set did not converge")
    return [a for a, _, _ in deferred]


def _score_wiki_scenario(out_json, nbytes, spec, known_sources):
    matches = out_json.get("matches", [])
    hay = "\n".join("%s %s" % (m.get("subject"), m.get("value"))
                    for m in matches).lower()
    res = {"context_bytes": nbytes,
           "conflicts_found": bool(out_json.get("conflicts"))}
    if spec.get("expect_gap"):
        res["answer_found"] = None
        res["gap_signaled"] = bool(out_json.get("gaps")) and not matches
        res["conflict_expected_met"] = None
    else:
        if spec.get("answer_any"):
            res["answer_found"] = any(
                a.lower() in hay for a in spec["answer_any"])
        elif spec.get("answer_all"):
            res["answer_found"] = all(
                a.lower() in hay for a in spec["answer_all"])
        else:
            res["answer_found"] = False
        res["gap_signaled"] = None
        conflict_expected = spec.get("conflict_expected")
        if conflict_expected is None:
            res["conflict_expected_met"] = None
        else:
            res["conflict_expected_met"] = (
                res["conflicts_found"] == conflict_expected)
    forbid = spec.get("forbid") or []
    top_value = str(matches[0].get("value")).lower() if matches else ""
    res["forbid_ok"] = all(f.lower() not in top_value for f in forbid)
    subject = spec.get("subject")
    res["subject_ok"] = (True if subject is None
                         else any(m["subject"] == subject for m in matches))
    res["traceable"] = all(
        ev.get("source_id") in known_sources
        for m in matches for ev in (m.get("evidence") or []))
    checks = [v for k, v in res.items()
              if k in ("answer_found", "gap_signaled",
                       "conflict_expected_met", "forbid_ok", "subject_ok",
                       "traceable") and v is not None]
    res["pass"] = all(checks)
    return res


def _tokens(text):
    return set(t for t in _TOKEN_RE.findall(text.lower())
               if len(t) >= 3 and t not in BSTOPWORDS)


def _baseline_scenario(files, spec):
    """files: list of (relpath, text). Keyword rank, read top 3."""
    q_toks = _tokens(spec["question"])
    scored = []
    for rel, text in files:
        ftoks = _tokens(text)
        scored.append((len(q_toks & ftoks), rel, text))
    scored.sort(key=lambda t: (-t[0], t[1]))
    top = scored[:_TOP3]
    hay = "\n".join(t[2] for t in top).lower()
    res = {"files_read": [t[1] for t in top],
           "context_bytes": sum(len(t[2].encode("utf-8")) for t in top),
           "gap_signaled": False}  # keyword search cannot signal gaps
    if spec.get("expect_gap"):
        res["answer_found"] = None
    elif spec.get("answer_any"):
        res["answer_found"] = any(a.lower() in hay for a in spec["answer_any"])
    elif spec.get("answer_all"):
        res["answer_found"] = all(a.lower() in hay for a in spec["answer_all"])
    else:
        res["answer_found"] = False
    # conflicts_found is measured, not gated: all answer_all strings
    # visible across the top-3 files, regardless of conflict_expected.
    wanted = spec.get("answer_all") or []
    res["conflicts_found"] = (len(wanted) >= 2
                              and all(w.lower() in hay for w in wanted))
    top_text = top[0][2].lower() if top else ""
    res["forbid_ok"] = all(f.lower() not in top_text
                           for f in (spec.get("forbid") or []))
    return res


def _engine_commit():
    try:
        p = subprocess.run(["git", "-C", REPO, "rev-parse", "HEAD"],
                           capture_output=True, text=True, timeout=10)
        sha = p.stdout.strip()
        if p.returncode == 0 and re.match(r"^[0-9a-f]{40}$", sha):
            return sha
    except Exception:
        pass
    return "unknown"


def run():
    tmp = tempfile.mkdtemp(prefix="llmwiki-bench-")
    try:
        return _run(tmp)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _run(tmp):
    scenarios = json.load(open(os.path.join(BENCH, "expectations.json")))["scenarios"]
    wiki_root = os.path.join(tmp, "proj")
    os.makedirs(wiki_root)

    # --- shared corpus copy ---------------------------------------------
    corpus_copy = os.path.join(tmp, "corpus")
    shutil.copytree(CORPUS, corpus_copy,
                    ignore=shutil.ignore_patterns("bench", "questions.txt"))
    shutil.copytree(BENCH, os.path.join(corpus_copy, "bench"))

    # --- wiki lane -------------------------------------------------------
    _cli(wiki_root, "init", "--name", "bench")
    known_sources = {}
    for rel in INGEST_ORDER:
        receipt, _ = _cli(wiki_root, "ingest", "--file",
                          os.path.join(corpus_copy, rel))
        sid, ver = receipt["source_id"], receipt["version"]
        known_sources[sid] = ver
        stem = os.path.splitext(os.path.basename(rel))[0]
        cfile = os.path.join(BENCH, "candidates", "%s.jsonl" % stem)
        if not os.path.isfile(cfile):
            continue
        staged, _ = _cli(wiki_root, "stage-candidates", "--source-id", sid,
                         "--source-version", str(ver), "--file", cfile)
        prep, _ = _cli(wiki_root, "reconcile-prepare", "--run", staged["run_id"])
        cls = _classify(prep, stem)
        cls_file = os.path.join(tmp, "cls-%s.json" % stem)
        with open(cls_file, "w") as f:
            json.dump(cls, f)
        _cli(wiki_root, "reconcile-apply", "--run", staged["run_id"],
             "--classifications", cls_file)

    wiki_dir = os.path.join(wiki_root, ".llm-wiki")
    claims = _load_claims(wiki_dir)
    written = _write_bench_pages(wiki_root, tmp, claims)
    _cli(wiki_root, "build-pages")
    verify, _ = _cli(wiki_root, "verify", ok_rcs=(0, 3))
    doctor, _ = _cli(wiki_root, "doctor", ok_rcs=(0, 3))

    wiki_scenarios = {}
    for spec in scenarios:
        argv = ["query-prepare", "--question", spec["question"]]
        if spec.get("as_of"):
            argv += ["--as-of", spec["as_of"]]
        out, nbytes = _cli(wiki_root, *argv)
        wiki_scenarios[spec["id"]] = _score_wiki_scenario(
            out, nbytes, spec, known_sources)

    warn_counts = collections.Counter(
        f["code"] for f in doctor["findings"] if f["severity"] == "warn")
    doctor_ok = all(warn_counts.get(code) == n
                    for code, n in EXPECTED_DOCTOR_WARNS.items())
    wiki_lane = {
        "scenarios": wiki_scenarios,
        "doctor": {"ok": doctor["ok"], "expected_warns_met": doctor_ok,
                   "warn_counts": dict(sorted(warn_counts.items()))},
        "verify_ok": verify["ok"],
        "pages_written": len(written),
        "claims_count": len(claims),
        "sources_count": len(known_sources),
    }

    # --- baseline lane ----------------------------------------------------
    bfiles = []
    for rel in BASELINE_FILES:
        with open(os.path.join(corpus_copy, rel), encoding="utf-8") as f:
            bfiles.append((rel, f.read()))
    baseline_scenarios = {spec["id"]: _baseline_scenario(bfiles, spec)
                          for spec in scenarios}

    # --- results ----------------------------------------------------------
    wiki_pass = (all(s["pass"] for s in wiki_scenarios.values())
                 and doctor_ok and verify["ok"])
    results = {
        "engine_commit": _engine_commit(),
        "pass": wiki_pass,
        "lanes": {"wiki": wiki_lane, "baseline": {"scenarios": baseline_scenarios}},
        "comparison": {
            "correct_answers": {
                "wiki": sum(1 for s in wiki_scenarios.values()
                            if s["answer_found"] or s["gap_signaled"]),
                "baseline": sum(1 for s in baseline_scenarios.values()
                                if s["answer_found"] or s["gap_signaled"]),
            },
            "conflicts_surfaced": {
                "wiki": sum(1 for s in wiki_scenarios.values()
                            if s["conflicts_found"]),
                "baseline": sum(1 for s in baseline_scenarios.values()
                                if s["conflicts_found"]),
            },
            "context_bytes_total": {
                "wiki": sum(s["context_bytes"] for s in wiki_scenarios.values()),
                "baseline": sum(s["context_bytes"]
                                for s in baseline_scenarios.values()),
            },
        },
    }
    with open(RESULTS_PATH, "w") as f:
        json.dump(results, f, indent=2, sort_keys=True)
        f.write("\n")
    return results


def _fmt(v):
    return "-" if v is None else ("yes" if v else "no")


def _table(results):
    w = results["lanes"]["wiki"]["scenarios"]
    b = results["lanes"]["baseline"]["scenarios"]
    lines = ["%-14s | %-22s | %-22s" % ("scenario", "wiki lane", "baseline lane"),
             "-" * 64,
             "%-14s | %-7s %-7s %-6s | %-7s %-7s %-6s"
             % ("", "answer", "conflict", "bytes", "answer", "conflict", "bytes"),
             "-" * 64]
    for sid in w:
        ws, bs = w[sid], b[sid]
        lines.append("%-14s | %-7s %-7s %-6d | %-7s %-7s %-6d" % (
            sid, _fmt(ws["answer_found"] if ws["answer_found"] is not None
                      else ws["gap_signaled"]),
            _fmt(ws["conflicts_found"]), ws["context_bytes"],
            _fmt(bs["answer_found"] if bs["answer_found"] is not None
                 else bs["gap_signaled"]),
            _fmt(bs["conflicts_found"]), bs["context_bytes"]))
    c = results["comparison"]
    lines += ["-" * 64,
              "answers: wiki %d | baseline %d   conflicts: wiki %d | baseline %d"
              "   context bytes: wiki %d | baseline %d"
              % (c["correct_answers"]["wiki"], c["correct_answers"]["baseline"],
                 c["conflicts_surfaced"]["wiki"],
                 c["conflicts_surfaced"]["baseline"],
                 c["context_bytes_total"]["wiki"],
                 c["context_bytes_total"]["baseline"])]
    doc = results["lanes"]["wiki"]["doctor"]
    lines.append("doctor expected warns: %s | verify ok: %s | overall: %s"
                 % (_fmt(doc["expected_warns_met"]),
                    _fmt(results["lanes"]["wiki"]["verify_ok"]),
                    "PASS" if results["pass"] else "FAIL"))
    return "\n".join(lines)


def main():
    results = run()
    print(_table(results))
    return 0 if results["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
