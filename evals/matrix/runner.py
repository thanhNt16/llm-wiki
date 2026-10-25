#!/usr/bin/env python3
"""Multi-engine evaluation matrix (spec §7): E1 version ladder + E6
determinism gate.

Engines are paths to extracted `wiki.py` files; historical commits are
materialized via `git archive <commit> skill/llm-wiki/scripts` into /tmp —
the same pattern the benchmark uses (the harness never imports wikicore
from the engine under test, it drives the CLI).

    python3 evals/matrix/runner.py --extract bb7a51f          # -> wiki.py path
    python3 evals/matrix/runner.py --engine base=bb7a51f \
        --engine skill-v2=f42e7bc --engine unified=HEAD

E1 = each engine x (golden corpus + synthetic(100)) producing the bench
metric set (correct/cited/forbidden-ok question scoring, context-pack
tokens, topology, two-source, hygiene, near-dup guard, wall seconds).
E6 = run E1's cells a second time and raise on any non-volatile metric
drift (wall_seconds and step stderr tails are stripped before compare).
"""
import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
BENCH_DIR = os.path.join(REPO, "evals", "benchmarks")
GOLDEN = os.path.join(REPO, "evals", "golden-corpus")
RESULTS_DIR = os.path.join(HERE, "results")

# Two import shapes: `python3 evals/matrix/runner.py` (flat) and
# `python3 -m unittest evals.matrix.test_matrix` (package).
if REPO not in sys.path:
    sys.path.insert(0, REPO)
if BENCH_DIR not in sys.path:
    sys.path.insert(0, BENCH_DIR)
try:
    from . import generator
except ImportError:
    sys.path.insert(0, HERE)
    import generator  # noqa: E402
try:
    from evals.benchmarks import driver, fixture, metrics
except ImportError:  # benchmarks dir on sys.path, flat module names
    import driver  # noqa: E402
    import fixture  # noqa: E402
    import metrics  # noqa: E402

VOLATILE_KEYS = {"wall_seconds", "elapsed", "duration_s", "err"}
_HEX40 = "0123456789abcdef"


# ---------------------------------------------------------------- engines

def extract_engine(commit, repo=REPO):
    """git archive <commit> skill/llm-wiki into /tmp; return the extracted
    wiki.py path (absolute). The whole skill dir rides along because
    wikicore resolves ../../schemas/ relative to scripts/."""
    dest = tempfile.mkdtemp(prefix="llmwiki-engine-%s-" % str(commit)[:12])
    arch = subprocess.run(["git", "-C", repo, "archive", commit,
                           "skill/llm-wiki"],
                          capture_output=True)
    if arch.returncode != 0:
        raise RuntimeError("git archive %s failed: %s"
                           % (commit, arch.stderr.decode("utf-8", "replace")))
    tar = subprocess.run(["tar", "-x", "-C", dest], input=arch.stdout,
                         capture_output=True)
    if tar.returncode != 0:
        raise RuntimeError("tar extract failed: %s"
                           % tar.stderr.decode("utf-8", "replace"))
    wiki_py = os.path.join(dest, "skill", "llm-wiki", "scripts", "wiki.py")
    if not os.path.isfile(wiki_py):
        raise RuntimeError("archive of %s has no skill/llm-wiki/scripts/wiki.py"
                           % commit)
    return wiki_py


def resolve_engine(spec):
    """--engine value: 'label=PATH' or 'label=COMMITISH'."""
    label, _, value = spec.partition("=")
    if not value:
        raise ValueError("--engine expects label=path|commit, got %r" % spec)
    if os.path.isfile(value):
        return label, os.path.abspath(value)
    return label, extract_engine(value)


# ---------------------------------------------------------------- metrics

def _json_or_none(engine, args, cwd):
    """engine.run + JSON parse; unlike engine.json this keeps stdout when
    the exit code is nonzero (doctor/verify exit 3 with findings)."""
    code, out, err = engine.run(args, cwd)
    try:
        return code, json.loads(out), err
    except ValueError:
        return code, None, err


def _triples(matches):
    return {(m.get("subject"), m.get("predicate"), m.get("value"))
            for m in matches}


def _question_metrics(engine, wiki_root, questions, known_source_ids):
    """correct / cited / forbidden-ok per gold question + aggregate
    precision/recall/MRR (mirrors metrics.retrieval, generic over any
    question list)."""
    hits, gold_total, rr_sum, refused_ok = 0, 0, 0.0, 0
    n_expect, n_refuse = 0, 0
    forbid_pass, forbid_n = 0, 0
    cited_pass, cited_n = 0, 0
    for q in questions:
        args = ["query-prepare", "--question", q["question"]]
        if q.get("as_of"):
            args += ["--as-of", q["as_of"]]
        code, out, _ = _json_or_none(engine, args, wiki_root)
        matches = ((out or {}).get("matches") or [])[:5]
        cited_n += 1
        if all(ev.get("source_id") in known_source_ids
               for m in matches for ev in (m.get("evidence") or [])):
            cited_pass += 1
        top_value = str(matches[0].get("value", "")).lower() if matches else ""
        forbid = q.get("forbid") or []
        if forbid:
            forbid_n += 1
            if all(f.lower() not in top_value for f in forbid):
                forbid_pass += 1
        if not q.get("expect"):
            n_refuse += 1
            if not matches:
                refused_ok += 1
            continue
        n_expect += 1
        gold = {(e["subject"], e["predicate"], e["value"])
                for e in q["expect"]}
        gold_total += len(gold)
        top = _triples(matches)
        hits += len(gold & top)
        for rank, m in enumerate(matches, 1):
            if (m.get("subject"), m.get("predicate"), m.get("value")) in gold:
                rr_sum += 1.0 / rank
                break
    return {
        "questions": len(questions),
        "correct": hits,
        "gold_total": gold_total,
        "gold_recall_at_5": None if not gold_total
        else round(hits / gold_total, 3),
        "precision_at_5": None if not n_expect
        else round(hits / (5 * n_expect), 3),
        "mrr": None if not n_expect else round(rr_sum / n_expect, 3),
        "refusals_correct": refused_ok == n_refuse,
        "cited": "%d/%d" % (cited_pass, cited_n),
        "forbidden_ok": "%d/%d" % (forbid_pass, forbid_n),
    }


def _context_metrics(engine, wiki_root, questions, corpus_dir):
    """context-pack receipts over the first 4 questions (mirrors
    metrics.context, generic over the question list)."""
    reqs, ests, compliant = 0, 0, 0
    for q in questions[:4]:
        code, out, _ = _json_or_none(
            engine, ["context-pack", "--task", q["question"],
                     "--budget", "6000"], wiki_root)
        receipt = (out or {}).get("receipt", {})
        est = receipt.get("budget", {}).get("estimated")
        req = receipt.get("budget", {}).get("requested", 6000)
        if est is None:
            continue
        reqs += 1
        ests += est
        compliant += 1 if est <= req else 0
    corpus_tokens = 0
    for path in glob.glob(os.path.join(corpus_dir, "**", "*"),
                          recursive=True):
        if os.path.isfile(path) and path.endswith((".md", ".json", ".csv")):
            with open(path, encoding="utf-8", errors="replace") as f:
                corpus_tokens += len(f.read().split())
    avg = (ests / reqs) if reqs else 0
    return {"pack_tokens_avg": round(avg, 1),
            "budget_compliance": None if not reqs else compliant == reqs,
            "compression_ratio": None if not avg
            else round(corpus_tokens / avg, 2)}


def _doctor_findings(engine, wiki_root):
    code, out, _ = _json_or_none(engine, ["doctor"], wiki_root)
    findings = (out or {}).get("findings") or []
    by_code = {}
    for f in findings:
        by_code[f["code"]] = by_code.get(f["code"], 0) + 1
    return by_code


def _claims_count(wiki_root):
    return len(glob.glob(os.path.join(
        wiki_root, ".llm-wiki", "claims", "claim_*.json")))


def _first_live_claim_link(wiki_root):
    """First non-terminal claim id as a [[link]], '' when none — the
    guard probe's dep (mirrors bench._current_claim_link)."""
    for cp in sorted(glob.glob(os.path.join(
            wiki_root, ".llm-wiki", "claims", "claim_*.json"))):
        with open(cp, encoding="utf-8") as f:
            c = json.load(f)
        if c.get("status") not in ("superseded", "rejected"):
            return "[[%s]]" % c["id"]
    return ""


def _concept_md(subject, title, link):
    return ("---\ntype: concept\nsubject: %s\ntitle: %s\n"
            "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
            % (subject, title, title, link or ""))


def _guard_probe(engine, wiki_root):
    """stray-page + near-duplicate-write probes (mirrors bench.py):
    near_dup_blocked iff the dup write exits 3 (TxnError)."""
    stray_path = os.path.join(
        wiki_root, ".llm-wiki", fixture.STRAY_PAGE_REL)
    os.makedirs(os.path.dirname(stray_path), exist_ok=True)
    with open(stray_path, "w", encoding="utf-8") as f:
        f.write(fixture.STRAY_PAGE_TEXT)

    link = _first_live_claim_link(wiki_root)
    orig_subject = " ".join(fixture.NEAR_DUP_SUBJECT.split()[:-1])
    orig_title = " ".join(fixture.NEAR_DUP_TITLE.split()[:-1])
    seed_code, _, _ = engine.run(
        ["write-page", "--text", _concept_md(orig_subject, orig_title, link),
         "--artifact", "wiki/concepts/%s.md"
         % orig_subject.replace(" ", "-")], wiki_root)
    nd_code, _, _ = engine.run(
        ["write-page", "--text",
         _concept_md(fixture.NEAR_DUP_SUBJECT, fixture.NEAR_DUP_TITLE, link),
         "--artifact", "wiki/concepts/%s.md"
         % fixture.NEAR_DUP_SUBJECT.replace(" ", "-")], wiki_root)
    by_code = _doctor_findings(engine, wiki_root)
    stray_flagged = (by_code.get("orphan_page", 0) > 0
                     or by_code.get("unlisted_page", 0) > 0)
    return {"near_dup_blocked": nd_code == 3,
            "seed_write_exit": seed_code,
            "near_dup_write_exit": nd_code,
            "stray_page_flagged": stray_flagged,
            "doctor_findings_by_code": by_code}


def apply_fault_pages(wiki_root, pages):
    """Write planted fault pages under .llm-wiki/ (filesystem injection —
    pages bypass write-page and the section index, which is exactly what
    doctor's lint tiers detect)."""
    written = []
    for pg in pages:
        dest = os.path.join(wiki_root, ".llm-wiki", pg["relpath"])
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        with open(dest, "w", encoding="utf-8") as f:
            f.write(pg["text"])
        written.append(pg["relpath"])
    return written


# ---------------------------------------------------------------- builders

def build_synthetic_wiki(engine, corpus_dir, wiki_root, manifest):
    """init -> ingest --dir -> stage+reconcile manifest claims (UNRELATED
    pass 1, deferred CORRECTION pass 2) -> build-pages -> verify."""
    steps = []

    def step(name, args, ok_codes=(0,)):
        t0 = time.time()
        code, out, err = engine.run(args, wiki_root)
        steps.append({"step": name, "exit": code,
                      "duration_s": round(time.time() - t0, 3),
                      "err": err.strip()[-200:] if code not in ok_codes else ""})
        return code, out

    code, _ = step("init", ["init", "--name", "matrix"])
    if code != 0:
        return {"steps": steps, "ok": False}

    corpus_abs = os.path.abspath(corpus_dir)
    code, out = step("ingest --dir", ["ingest", "--dir", corpus_abs])
    src_ids = {}
    if code == 0:
        try:
            receipt = json.loads(out)
            for item in receipt.get("items", []):
                if item.get("source_id"):
                    src_ids[item["path"]] = (item["source_id"],
                                             item.get("version") or 1)
        except ValueError:
            steps.append({"step": "parse ingest --dir", "exit": 1,
                          "err": "non-JSON stdout"})
            return {"steps": steps, "ok": False}
    else:
        # engines without --dir: per-file fallback in walk order
        for dirpath, _d, files in os.walk(corpus_abs):
            for name in sorted(files):
                rel = os.path.relpath(os.path.join(dirpath, name), corpus_abs)
                code, out = step("ingest " + rel,
                                 ["ingest", "--file",
                                  os.path.join(corpus_abs, rel)])
                if code != 0:
                    return {"steps": steps, "ok": False}
                try:
                    r = json.loads(out)
                    src_ids[rel.replace(os.sep, "/")] = (
                        r["source_id"], r.get("version") or 1)
                except ValueError:
                    steps.append({"step": "parse ingest " + rel, "exit": 1,
                                  "err": "non-JSON stdout"})
                    return {"steps": steps, "ok": False}

    deferred = []
    for rel in sorted(manifest["sources"]):
        claims = manifest["sources"][rel].get("claims") or []
        if not claims:
            continue
        if rel not in src_ids:
            steps.append({"step": "manifest rel missing " + rel, "exit": 1,
                          "err": "source not in ingest receipt"})
            return {"steps": steps, "ok": False, "source_ids": src_ids}
        sid, ver = src_ids[rel]
        cands, cls = [], []
        for c in claims:
            c = json.loads(json.dumps(c))
            for ev in c["evidence"]:
                ev["source_id"] = sid
                ev["source_version"] = ver
            if c.pop("defer", None) is not None:
                deferred.append((rel, sid, c))
                continue
            for ev in c["evidence"]:
                ev["source_id"] = sid
                ev["source_version"] = ver
            sup = [s for s in c["supersedes"] if not s.startswith("AUTO:")]
            c["supersedes"] = sup
            cls.append({"index": len(cands), "relationship": "UNRELATED",
                        "target_claim_id": None, "valid_from": None,
                        "valid_to": None,
                        "authority": {"type": c["authority"]["type"]}})
            cands.append(c)
        if not cands:
            continue
        if not driver._stage_and_reconcile(engine, wiki_root, steps, step,
                                           sid, rel, cands, cls):
            return {"steps": steps, "ok": False, "source_ids": src_ids}

    # pass 2: deferred corrections (supersedes AUTO:<subject|pred|value>)
    for rel, sid, cand in deferred:
        auto = [s for s in cand["supersedes"] if s.startswith("AUTO:")]
        target_key = auto[0][len("AUTO:"):] if auto else None
        target_id = _find_claim_id(wiki_root, target_key)
        if target_id is None:
            steps.append({"step": "correction lookup " + rel, "exit": 3,
                          "err": "no target claim for %s" % target_key})
            return {"steps": steps, "ok": False, "source_ids": src_ids}
        cand["supersedes"] = []
        cls = [{"index": 0, "relationship": "CORRECTION",
                "target_claim_id": target_id,
                "valid_from": cand.get("valid_from"), "valid_to": None,
                "authority": {"type": cand["authority"]["type"]}}]
        if not driver._stage_and_reconcile(engine, wiki_root, steps, step,
                                           sid, rel + " correction",
                                           [cand], cls):
            return {"steps": steps, "ok": False, "source_ids": src_ids}

    step("build-pages", ["build-pages"])
    code, out = step("verify", ["verify"], ok_codes=(0, 3))
    ok = False
    try:
        ok = json.loads(out).get("ok", False) if out else False
    except ValueError:
        ok = False
    ok = ok and code in (0, 3)
    return {"steps": steps, "ok": ok, "source_ids": src_ids}


# ---------------------------------------------------------------- scenarios

def _scenario(spec):
    """'golden' | 'synthetic:N' | 'synthetic:N:SEED' | '+fault' suffix."""
    fault = None
    if "+" in spec:
        spec, fault = spec.rsplit("+", 1)
        if fault not in generator.FAULTS:
            raise ValueError("unknown fault %r (expected one of %s)"
                             % (fault, ", ".join(generator.FAULTS)))
    if spec == "golden":
        return {"kind": "golden", "fault": fault}
    if spec.startswith("synthetic"):
        parts = spec.split(":")
        n = int(parts[1]) if len(parts) > 1 else 100
        seed = int(parts[2]) if len(parts) > 2 else 42
        return {"kind": "synthetic", "n": n, "seed": seed, "fault": fault}
    raise ValueError("unknown scenario spec: %r" % spec)


def _scenario_name(spec):
    if spec["kind"] == "golden":
        base = "golden"
    else:
        base = "synthetic-%d-s%d" % (spec["n"], spec["seed"])
    return base + ("+" + spec["fault"] if spec.get("fault") else "")


def _run_cell(label, engine_path, spec):
    """One engine x scenario cell -> metric dict."""
    engine = driver.Engine(engine_path)
    wiki_root = tempfile.mkdtemp(prefix="llmwiki-matrix-")
    t0 = time.time()
    try:
        if spec["kind"] == "golden":
            corpus = GOLDEN
            questions = fixture.GOLD_QUESTIONS
            manifest = None
            build = driver.build_wiki(engine, corpus, wiki_root)
        else:
            corpus_dir = generator.synthetic_corpus(
                spec["n"], spec.get("seed", 42), fault=spec.get("fault"))
            with open(os.path.join(corpus_dir, "manifest.json")) as f:
                manifest = json.load(f)
            corpus = os.path.join(corpus_dir, manifest["corpus_dir"])
            questions = manifest["gold_questions"]
            build = build_synthetic_wiki(engine, corpus, wiki_root, manifest)
        wall = round(time.time() - t0, 3)

        cell = {"engine": label, "scenario": _scenario_name(spec),
                "build_ok": bool(build.get("ok")),
                "build_steps": build.get("steps", []),
                "wall_seconds": wall,
                "claims_count": _claims_count(wiki_root)}
        if not build.get("ok"):
            cell["error"] = "build failed"
            return cell

        source_ids = build.get("source_ids") or {}
        if spec["kind"] == "synthetic":
            known_sids = {sid for sid, _v in source_ids.values()}
        else:
            known_sids = set(source_ids.values())

        if manifest and manifest.get("fault_pages"):
            cell["fault_pages_applied"] = apply_fault_pages(
                wiki_root, manifest["fault_pages"])
            findings = _doctor_findings(engine, wiki_root)
            cell["fault_detection"] = {
                code: findings.get(code, 0)
                for code in manifest.get("expected_findings", [])}

        cell["questions"] = _question_metrics(engine, wiki_root,
                                              questions, known_sids)
        cell["context"] = _context_metrics(engine, wiki_root,
                                           questions, corpus)
        cell["topology"] = metrics.topology(wiki_root)
        cell["twosource"] = metrics.twosource(engine, wiki_root)
        cell["guard"] = _guard_probe(engine, wiki_root)
        return cell
    finally:
        shutil.rmtree(wiki_root, ignore_errors=True)


# ---------------------------------------------------------------- experiments
# E2 scale sweep, E4 failure drills, E3 maintenance sim (spec §7).
# E2 sizes and the E3 cycle count are overridable for smoke runs.

E2_SIZES = (100, 300, 1000)
E3_N = 300
E3_CYCLES = 10
E4_N = 100

# Data-driven E3 maintenance cycle (spec §7): every cycle is this same
# op template with per-cycle parameters filled from the cycle index.
# ops are (kind, params) pairs executed in order through the driver.
def _e3_cycle_ops(c, rng):
    """One maintenance cycle: ingest a new source -> stage a claim
    ("compile") -> supersede it ("correction") -> build-pages ->
    filesystem churn (unlisted stale page + unlisted orphan) -> lint ->
    repair -> assert query. Parameters are deterministic in c."""
    word = generator.TOPICS[c % len(generator.TOPICS)]
    subject = "maintenance %s c%d" % (word, c)
    predicate = generator.PREDICATES[c % len(generator.PREDICATES)]
    return [
        ("ingest", {"relpath": "docs/maint-%03d.md" % c,
                    "subject": subject, "predicate": predicate}),
        ("compile", {"subject": subject, "predicate": predicate,
                     "value": "%s-c%d-draft" % (predicate, c)}),
        ("correction", {"subject": subject, "predicate": predicate,
                        "from": "%s-c%d-draft" % (predicate, c),
                        "to": "%s-c%d-final" % (predicate, c)}),
        ("recompile", {}),
        ("churn", {"pages": [
            {"relpath": generator._rel("maint-cyc-%d" % c),
             "stale_after": "2020-02-%02d" % (c + 1)},
            {"relpath": generator._rel("maint-orphan-%d" % c)},
        ]}),
        ("lint", {}),
        ("repair", {}),
        ("assert_query", {"subject": subject, "predicate": predicate,
                          "expect": "%s-c%d-final" % (predicate, c),
                          "forbid": "%s-c%d-draft" % (predicate, c)}),
    ]


def _find_claim_id(wiki_root, target_key):
    """claim id whose subject|predicate|value equals target_key, None."""
    if not target_key:
        return None
    for cp in sorted(glob.glob(os.path.join(
            wiki_root, ".llm-wiki", "claims", "claim_*.json"))):
        with open(cp) as f:
            c = json.load(f)
        if target_key == "%s|%s|%s" % (c.get("subject"),
                                     c.get("predicate"),
                                     c.get("value")):
            return c["id"]
    return None


def _timed_doctor(engine, wiki_root, extra=None):
    """doctor run with wall-clock timing; returns (seconds, result|None)."""
    t0 = time.time()
    code, out, _ = _json_or_none(engine, ["doctor"] + list(extra or ()),
                                 wiki_root)
    return round(time.time() - t0, 3), out, code


def _phase_seconds(steps):
    """Bucket build-step durations by first word: ingest/stage/reconcile/
    build-pages/verify sums for the E2 phase table."""
    phases = {}
    for s in steps:
        phase = s["step"].split(" ", 1)[0]
        phases[phase] = round(phases.get(phase, 0.0) + s.get("duration_s", 0.0),
                              3)
    return phases


# -- E2 scale sweep --------------------------------------------------------

def _e2_cell(label, engine_path, n, seed):
    """unified HEAD x synthetic(N): build-phase wall times, a timed doctor
    sweep, context-pack tokens, and question metrics. No guard probes —
    the lint sweep measures the clean synthetic wiki."""
    engine = driver.Engine(engine_path)
    wiki_root = tempfile.mkdtemp(prefix="llmwiki-matrix-e2-")
    try:
        corpus_dir = generator.synthetic_corpus(n, seed)
        with open(os.path.join(corpus_dir, "manifest.json")) as f:
            manifest = json.load(f)
        corpus = os.path.join(corpus_dir, manifest["corpus_dir"])
        build = build_synthetic_wiki(engine, corpus, wiki_root, manifest)
        steps = build.get("steps", [])
        cell = {"engine": label, "n": n, "seed": seed,
                "build_ok": bool(build.get("ok")),
                "wall_seconds": round(
                    sum(s.get("duration_s", 0.0) for s in steps), 3),
                "phase_seconds": _phase_seconds(steps),
                "steps": steps}
        if not build.get("ok"):
            cell["error"] = "build failed"
            return cell
        known_sids = {sid for sid, _v in (build.get("source_ids") or {}).values()}
        cell["claims_count"] = _claims_count(wiki_root)
        lint_s, doc, _ = _timed_doctor(engine, wiki_root)
        cell["lint_sweep_seconds"] = lint_s
        cell["lint_findings"] = (doc or {}).get("counts")
        cell["questions"] = _question_metrics(
            engine, wiki_root, manifest["gold_questions"], known_sids)
        cell["context"] = _context_metrics(
            engine, wiki_root, manifest["gold_questions"], corpus)
        return cell
    finally:
        shutil.rmtree(wiki_root, ignore_errors=True)


# -- E4 failure drills -----------------------------------------------------

# Fault-injected pages that carry a finding themselves (hubs/spines are
# planted support pages — the finding lands on the garden/twin/stale/
# orphan pages). Maps fault -> slug prefixes that are lint targets.
_E4_LINT_TARGETS = {
    "near_dup_flood": ("nd-a-", "nd-b-", "nd-c-"),
    "orphan_farm": ("orphan-farm-",),
    "walled_garden": ("wg-garden-",),
    "stale_ttl_farm": ("stale-farm-",),
}


def _planted_targets(pages, fault):
    """relpaths of injected pages an expected-code finding should name."""
    prefixes = _E4_LINT_TARGETS[fault]
    return sorted(p["relpath"] for p in pages
                  if os.path.basename(p["relpath"]).startswith(prefixes))


def _detected_paths(findings, codes, planted):
    """Planted paths named by >=1 expected-code finding; non-planted
    expected-code hits count as false positives."""
    detected, false_pos = set(), []
    for f in findings:
        if f.get("code") not in codes:
            continue
        hit = [p for p in planted if p in f.get("detail", "")]
        if hit:
            detected.update(hit)
        else:
            false_pos.append(f.get("detail", ""))
    return detected, false_pos


def _repair_plan_coverage(findings, plan, codes):
    """share of expected-code findings with a repair_plan entry whose path
    is named in the finding detail (plan rows are (code, path, action))."""
    if not plan:
        return None
    by_code = {}
    for p in plan:
        by_code.setdefault(p.get("code"), []).append(p.get("path", ""))
    covered = total = 0
    for f in findings:
        if f.get("code") not in codes:
            continue
        total += 1
        if any(path and path in f.get("detail", "")
               for path in by_code.get(f["code"], [])):
            covered += 1
    return {"covered": covered, "total": total,
            "coverage": round(covered / total, 3) if total else None}


def _e4_guard_drill(engine, wiki_root, pages):
    """near_dup_flood write guard drill: one new-page write attempt per
    planted twin subject ('<subject> notes' clears the Jaccard write
    threshold vs the twin pair). Blocked iff write-page exits 3."""
    subjects = []
    for pg in pages:
        slug = os.path.basename(pg["relpath"])[:-3]
        if slug.startswith(("nd-a-", "nd-b-", "nd-c-")):
            # the real twin subject lives in the injected frontmatter
            subject = next(
                (ln.split(":", 1)[1].strip()
                 for ln in pg["text"].splitlines()
                 if ln.startswith("subject:")), None)
            if subject and subject not in subjects:
                subjects.append(subject)
    link = _first_live_claim_link(wiki_root)
    probes = []
    for subject in subjects:
        probe = "%s notes" % subject
        title = probe.title()
        code, _out, _err = engine.run(
            ["write-page", "--text", _concept_md(probe, title, link),
             "--artifact", "wiki/concepts/%s.md" % probe.replace(" ", "-")],
            wiki_root)
        probes.append({"subject": probe, "exit": code})
    blocked = sum(1 for p in probes if p["exit"] == 3)
    return {"probes": probes, "attempted": len(probes), "blocked": blocked,
            "block_rate": round(blocked / len(probes), 3) if probes else None}


def _e4_cell(label, engine_path, fault, n=100, seed=42):
    """One engine x fault fixture: lint recall = planted pages detected /
    planted, repair-plan coverage, expected-code false positives, and the
    near-dup write-guard drill on the near_dup_flood fixture."""
    engine = driver.Engine(engine_path)
    wiki_root = tempfile.mkdtemp(prefix="llmwiki-matrix-e4-")
    try:
        corpus_dir = generator.synthetic_corpus(n, seed, fault=fault)
        with open(os.path.join(corpus_dir, "manifest.json")) as f:
            manifest = json.load(f)
        corpus = os.path.join(corpus_dir, manifest["corpus_dir"])
        build = build_synthetic_wiki(engine, corpus, wiki_root, manifest)
        cell = {"engine": label, "fault": fault, "n": n,
                "build_ok": bool(build.get("ok"))}
        if not build.get("ok"):
            cell["error"] = "build failed"
            cell["build_steps"] = build.get("steps", [])
            return cell
        planted = _planted_targets(manifest["fault_pages"], fault)
        apply_fault_pages(wiki_root, manifest["fault_pages"])
        _lint_s, doc, _ = _timed_doctor(engine, wiki_root)
        findings = (doc or {}).get("findings") or []
        codes = manifest["expected_findings"]
        detected, false_pos = _detected_paths(findings, codes, planted)
        cell["planted"] = len(planted)
        cell["detected"] = len(detected)
        cell["lint_recall"] = (round(len(detected) / len(planted), 3)
                               if planted else None)
        cell["false_positives"] = len(false_pos)
        cell["findings_by_code"] = {
            c: sum(1 for f in findings if f.get("code") == c)
            for c in sorted({f.get("code") for f in findings})}
        cell["repair_plan"] = _repair_plan_coverage(
            findings, (doc or {}).get("repair_plan"), codes)
        if fault == "near_dup_flood":
            cell["guard_drill"] = _e4_guard_drill(
                engine, wiki_root, manifest["fault_pages"])
        return cell
    finally:
        shutil.rmtree(wiki_root, ignore_errors=True)


def _e4_clean(label, engine_path, n=100, seed=42):
    """Clean-corpus doctor sweep: warn/error findings an engine raises
    with NO faults planted — the E4 false-positive floor."""
    engine = driver.Engine(engine_path)
    wiki_root = tempfile.mkdtemp(prefix="llmwiki-matrix-e4clean-")
    try:
        corpus_dir = generator.synthetic_corpus(n, seed)
        with open(os.path.join(corpus_dir, "manifest.json")) as f:
            manifest = json.load(f)
        corpus = os.path.join(corpus_dir, manifest["corpus_dir"])
        build = build_synthetic_wiki(engine, corpus, wiki_root, manifest)
        cell = {"engine": label, "fault": "clean", "n": n,
                "build_ok": bool(build.get("ok"))}
        if not build.get("ok"):
            cell["error"] = "build failed"
            return cell
        _s, doc, _ = _timed_doctor(engine, wiki_root)
        findings = (doc or {}).get("findings") or []
        by_code = {}
        for f in findings:
            if f.get("severity") in ("warn", "error"):
                by_code[f["code"]] = by_code.get(f["code"], 0) + 1
        cell["clean_false_positives_by_code"] = by_code
        cell["clean_false_positives"] = sum(by_code.values())
        return cell
    finally:
        shutil.rmtree(wiki_root, ignore_errors=True)


# -- E3 maintenance sim ----------------------------------------------------

def _e3_exec(engine, wiki_root, op, last_ingest, steps):
    """Execute one E3 cycle op through the engine CLI (or the filesystem
    for churn). Returns a small result dict merged into the cycle record.
    """
    kind, params = op

    def step(name, args, ok_codes=(0,)):
        t0 = time.time()
        code, out, err = engine.run(args, wiki_root)
        steps.append({"step": name, "exit": code,
                      "duration_s": round(time.time() - t0, 3),
                      "err": err.strip()[-200:] if code not in ok_codes else ""})
        return code, out

    if kind == "ingest":
        path = os.path.join(wiki_root, ".e3-corpus", params["relpath"])
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write("# %s\n\nSubject: %s.\n\nThe %s %s is set to %s.\n"
                    % (params["relpath"], params["subject"],
                       params["subject"], params["predicate"],
                       params["predicate"] + "-v0"))
        code, out = step("ingest " + params["relpath"],
                         ["ingest", "--file", path])
        try:
            r = json.loads(out)
            last_ingest["sid"] = r.get("source_id")
            last_ingest["ver"] = r.get("version") or 1
        except ValueError:
            last_ingest["sid"] = None
        return {"ingest_exit": code, "fs_writes": 1}

    if kind in ("compile", "correction"):
        sid, ver = last_ingest.get("sid"), last_ingest.get("ver")
        if sid is None:
            return {"op_error": "no ingested source"}
        if kind == "compile":
            cand = generator._claim(params["subject"], params["predicate"],
                                    params["value"])
            cls_rel = "UNRELATED"
            target_id = None
        else:
            cand = generator._claim(params["subject"], params["predicate"],
                                    params["to"])
            cls_rel = "CORRECTION"
            target_id = _find_claim_id(
                wiki_root, "%s|%s|%s" % (params["subject"],
                                         params["predicate"],
                                         params["from"]))
            if target_id is None:
                return {"op_error": "no claim to correct"}
        for ev in cand["evidence"]:
            ev["source_id"] = sid
            ev["source_version"] = ver
        cand["supersedes"] = []
        cls = [{"index": 0, "relationship": cls_rel,
                "target_claim_id": target_id, "valid_from": None,
                "valid_to": None,
                "authority": {"type": cand["authority"]["type"]}}]
        ok = driver._stage_and_reconcile(engine, wiki_root, steps, step,
                                         sid, kind, [cand], cls)
        return {kind + "_ok": ok}

    if kind == "recompile":
        code, _ = step("build-pages", ["build-pages"])
        return {"recompile_exit": code}

    if kind == "churn":
        written = []
        for pg in params["pages"]:
            extra = ("stale_after: %s\n" % pg["stale_after"]
                     if pg.get("stale_after") else "")
            subject = "cycle churn " + os.path.basename(pg["relpath"])[:-3]
            dest = os.path.join(wiki_root, ".llm-wiki", pg["relpath"])
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "w", encoding="utf-8") as f:
                f.write(generator._page(subject, subject.title(),
                                        "Maintenance-cycle churn page.",
                                        extra_front=extra))
            written.append(pg["relpath"])
        return {"churned": written, "fs_writes": len(written)}

    if kind == "lint":
        _s, doc, code = _timed_doctor(engine, wiki_root)
        return {"lint_exit": code, "lint": doc,
                "lint_seconds": _s}

    if kind == "repair":
        _s, doc, code = _timed_doctor(engine, wiki_root, ["--fix-indexes"])
        return {"repair_exit": code, "repair": doc}

    if kind == "assert_query":
        q = {"id": "e3-cycle", "question": "What is the %s %s?"
             % (params["subject"], params["predicate"]),
             "expect": [{"subject": params["subject"],
                         "predicate": params["predicate"],
                         "value": params["expect"]}],
             "forbid": [params["forbid"]]}
        res = _question_metrics(engine, wiki_root, [q], set())
        return {"query_ok": res["correct"] == 1,
                "forbidden_ok": res["forbidden_ok"] == "1/1"}

    raise ValueError("unknown E3 op kind: %r" % kind)


def _e3_cell(label, engine_path, n=E3_N, cycles=E3_CYCLES, seed=42):
    """Maintenance sim: `cycles` rounds of the data-driven cycle script on
    one synthetic(N) wiki. Per cycle: staleness caught/missed, findings
    before vs after repair, and engine/fs ops count."""
    engine = driver.Engine(engine_path)
    wiki_root = tempfile.mkdtemp(prefix="llmwiki-matrix-e3-")
    try:
        corpus_dir = generator.synthetic_corpus(n, seed)
        with open(os.path.join(corpus_dir, "manifest.json")) as f:
            manifest = json.load(f)
        corpus = os.path.join(corpus_dir, manifest["corpus_dir"])
        build = build_synthetic_wiki(engine, corpus, wiki_root, manifest)
        cell = {"engine": label, "n": n, "cycles_planned": cycles,
                "build_ok": bool(build.get("ok"))}
        if not build.get("ok"):
            cell["error"] = "build failed"
            cell["build_steps"] = build.get("steps", [])
            return cell

        cycle_rows = []
        last_ingest = {}
        for c in range(cycles):
            steps = []
            row = {"cycle": c, "fs_writes": 0}
            for op in _e3_cycle_ops(c, None):
                res = _e3_exec(engine, wiki_root, op, last_ingest, steps)
                row.update(res)
            lint = row.pop("lint", None) or {}
            repair = row.pop("repair", None) or {}
            findings = lint.get("findings") or []
            stale_page = "wiki/concepts/maint-cyc-%d.md" % c
            stale_hits = [f for f in findings
                          if f.get("code") == "stale_page_ttl"
                          and stale_page in f.get("detail", "")]
            row["staleness_caught"] = len(stale_hits)
            row["staleness_missed"] = 1 - min(1, len(stale_hits))
            row["findings_before"] = len(findings)
            row["findings_after"] = len(repair.get("findings") or [])
            row["repaired"] = repair.get("repaired")
            row["ops"] = len(steps) + row.pop("fs_writes", 0)
            row["steps"] = steps
            cycle_rows.append(row)
        cell["cycles"] = cycle_rows
        cell["staleness_caught_total"] = sum(r["staleness_caught"]
                                             for r in cycle_rows)
        cell["staleness_missed_total"] = sum(r["staleness_missed"]
                                             for r in cycle_rows)
        cell["query_ok_total"] = sum(1 for r in cycle_rows
                                     if r.get("query_ok"))
        cell["forbidden_ok_total"] = sum(1 for r in cycle_rows
                                         if r.get("forbidden_ok"))
        return cell
    finally:
        shutil.rmtree(wiki_root, ignore_errors=True)
# ---------------------------------------------------------------- matrix

def _strip_volatile(obj):
    """Recursively drop keys whose values legitimately vary run-to-run
    (wall clock, stderr tails) so E6 compares only deterministic signal."""
    if isinstance(obj, dict):
        return {k: _strip_volatile(v) for k, v in sorted(obj.items())
                if k not in VOLATILE_KEYS}
    if isinstance(obj, list):
        return [_strip_volatile(v) for v in obj]
    return obj


def _diff(a, b, path=""):
    """First divergence between two stripped structures, for the E6 error."""
    if type(a) is not type(b):
        return "%s: %r vs %r" % (path or "<root>", a, b)
    if isinstance(a, dict):
        if set(a) != set(b):
            return "%s: keys %s vs %s" % (
                path or "<root>", sorted(set(a) - set(b)),
                sorted(set(b) - set(a)))
        for k in a:
            d = _diff(a[k], b[k], "%s.%s" % (path, k))
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return "%s: len %d vs %d" % (path, len(a), len(b))
        for i, (x, y) in enumerate(zip(a, b)):
            d = _diff(x, y, "%s[%d]" % (path, i))
            if d:
                return d
        return None
    return None if a == b else "%s: %r vs %r" % (path, a, b)


def _exp_engines(engine_refs, want):
    """Subset engine_refs to `want` labels; absent wants are skipped so a
    partial CLI selection degrades honestly."""
    return {l: engine_refs[l] for l in want if l in engine_refs}


def run(engine_refs, scenarios=None, experiments=None,
        e2_sizes=None, e3_cycles=None):
    """engine_refs: {label: wiki.py path}. scenarios: spec strings or
    dicts; experiments: subset of ("E1", "E2", "E3", "E4", "E6").
    E2 sweeps synthetic sizes on the 'unified' engine (all engines when
    no 'unified' label); E4 runs the four fault fixtures plus a clean
    corpus per engine; E3 runs the maintenance sim on 'unified' (all
    engines otherwise). Returns the matrix dict."""
    scenarios = scenarios or ["golden", "synthetic:100"]
    experiments = experiments or ["E1"]
    specs = [_scenario(s) if isinstance(s, str) else s for s in scenarios]

    def cells():
        out = {}
        for label in sorted(engine_refs):
            for spec in specs:
                name = _scenario_name(spec)
                out["%s/%s" % (label, name)] = _run_cell(
                    label, engine_refs[label], spec)
        return out

    def _want_unified_or_all():
        return sorted(_exp_engines(engine_refs, ("unified",))
                      or engine_refs)

    matrix = {"experiment": "+".join(experiments),
              "engine_refs": dict(sorted(engine_refs.items())),
              "date": time.strftime("%Y-%m-%d", time.gmtime()),
              "experiments": {}}

    first = cells() if set(experiments) & {"E1", "E6"} else {}
    if "E1" in experiments:
        matrix["experiments"]["E1"] = {"cells": first}
    if "E2" in experiments:
        out = {}
        for label in _want_unified_or_all():
            for n in (e2_sizes or E2_SIZES):
                out["%s/synthetic-%d-s42" % (label, n)] = _e2_cell(
                    label, engine_refs[label], n, 42)
        matrix["experiments"]["E2"] = {"sizes": list(e2_sizes or E2_SIZES),
                                       "cells": out}
    if "E4" in experiments:
        out = {}
        for label in sorted(engine_refs):
            out["%s/clean" % label] = _e4_clean(label, engine_refs[label])
            for fault in generator.FAULTS:
                out["%s/%s" % (label, fault)] = _e4_cell(
                    label, engine_refs[label], fault)
        matrix["experiments"]["E4"] = {"faults": list(generator.FAULTS),
                                       "cells": out}
    if "E3" in experiments:
        out = {}
        for label in _want_unified_or_all():
            out["%s/maintenance" % label] = _e3_cell(
                label, engine_refs[label], cycles=e3_cycles or E3_CYCLES)
        matrix["experiments"]["E3"] = {
            "cycles": e3_cycles or E3_CYCLES, "n": E3_N, "cells": out}
    if "E6" in experiments:
        second = cells()
        a, b = _strip_volatile(first), _strip_volatile(second)
        drift = []
        for cell_name in sorted(set(a) | set(b)):
            d = _diff(a.get(cell_name), b.get(cell_name), cell_name)
            if d:
                drift.append(d)
        matrix["experiments"]["E6"] = {
            "deterministic": not drift,
            "drift": drift,
            "cells_verified": sorted(set(a) & set(b)),
        }
        if drift:
            raise RuntimeError("E6 determinism drift: %s" % "; ".join(drift))
    return matrix


def summarize(matrix):
    """Human-readable per-engine metric table for reports."""
    lines = []
    cells = matrix.get("experiments", {}).get("E1", {}).get("cells", {})
    header = ("cell", "ok", "correct", "recall@5", "mrr", "refusals",
              "cited", "forbid", "pages", "near_dup_blocked", "claims")
    lines.append("%-42s %3s %7s %8s %5s %8s %9s %7s %5s %16s %6s" % header)
    for name in sorted(cells):
        c = cells[name]
        q = c.get("questions") or {}
        g = c.get("guard") or {}
        t = c.get("topology") or {}
        lines.append("%-42s %3s %7s %8s %5s %8s %9s %7s %5s %16s %6s" % (
            name, "y" if c.get("build_ok") else "n",
            q.get("correct", "-"), q.get("gold_recall_at_5", "-"),
            q.get("mrr", "-"), q.get("refusals_correct", "-"),
            q.get("cited", "-"), q.get("forbidden_ok", "-"),
            t.get("pages", "-"), g.get("near_dup_blocked", "-"),
            c.get("claims_count", "-")))
    e2 = matrix.get("experiments", {}).get("E2")
    if e2:
        lines.append("E2 scale sweep:")
        lines.append("  %-28s %4s %8s %6s %9s %8s %6s" % (
            "cell", "n", "build_s", "lint_s", "pack_toks",
            "recall@5", "mrr"))
        for name in sorted(e2.get("cells", {})):
            c = e2["cells"][name]
            q = c.get("questions") or {}
            ctx = c.get("context") or {}
            lines.append("  %-28s %4s %8s %6s %9s %8s %6s" % (
                name, c.get("n", "-"), c.get("wall_seconds", "-"),
                c.get("lint_sweep_seconds", "-"),
                ctx.get("pack_tokens_avg", "-"),
                q.get("gold_recall_at_5", "-"), q.get("mrr", "-")))
    e4 = matrix.get("experiments", {}).get("E4")
    if e4:
        lines.append("E4 failure drills:")
        lines.append("  %-34s %7s %4s %6s %9s %10s" % (
            "cell", "recall", "FPs", "cov", "guard_blk", "clean_FP"))
        for name in sorted(e4.get("cells", {})):
            c = e4["cells"][name]
            plan = c.get("repair_plan") or {}
            drill = c.get("guard_drill") or {}
            lines.append("  %-34s %7s %4s %6s %9s %10s" % (
                name, c.get("lint_recall",
                            c.get("clean_false_positives", "-")),
                c.get("false_positives", "-"),
                plan.get("coverage", "-"), drill.get("block_rate", "-"),
                c.get("clean_false_positives", "")))
    e3 = matrix.get("experiments", {}).get("E3")
    if e3:
        for name in sorted(e3.get("cells", {})):
            c = e3["cells"][name]
            lines.append("E3 maintenance %s: cycles=%d caught=%d missed=%d "
                         "query_ok=%d forbid_ok=%d" % (
                             name, len(c.get("cycles", [])),
                             c.get("staleness_caught_total", 0),
                             c.get("staleness_missed_total", 0),
                             c.get("query_ok_total", 0),
                             c.get("forbidden_ok_total", 0)))
    e6 = matrix.get("experiments", {}).get("E6")
    if e6:
        lines.append("E6 determinism: %s (%d cells)"
                     % ("PASS" if e6["deterministic"] else "DRIFT",
                        len(e6.get("cells_verified", []))))
    return "\n".join(lines)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--extract", metavar="COMMIT",
                    help="git archive COMMIT's engine into /tmp; print the "
                         "wiki.py path and exit")
    ap.add_argument("--engine", action="append", default=[],
                    metavar="LABEL=PATH|COMMIT")
    ap.add_argument("--scenario", action="append", default=None,
                    help="golden | synthetic:N[:SEED][+fault]")
    ap.add_argument("--experiment", action="append", default=None,
                    choices=["E1", "E2", "E3", "E4", "E6"])
    ap.add_argument("--sizes", default=None, metavar="N,N,N",
                    help="E2 synthetic sizes (default %s)"
                    % ",".join(map(str, E2_SIZES)))
    ap.add_argument("--cycles", type=int, default=None,
                    help="E3 maintenance cycles (default %d)" % E3_CYCLES)
    ap.add_argument("--out", default=RESULTS_DIR)
    args = ap.parse_args(argv)

    if args.extract:
        print(extract_engine(args.extract))
        return 0

    engine_refs = {}
    for spec in args.engine:
        label, path = resolve_engine(spec)
        engine_refs[label] = path
    if not engine_refs:
        engine_refs["unified"] = os.path.join(
            REPO, "skill", "llm-wiki", "scripts", "wiki.py")

    e2_sizes = ([int(x) for x in args.sizes.split(",") if x.strip()]
                if args.sizes else None)
    experiments = args.experiment or ["E1", "E6"]
    matrix = run(engine_refs, scenarios=args.scenario,
                 experiments=experiments,
                 e2_sizes=e2_sizes, e3_cycles=args.cycles)
    os.makedirs(args.out, exist_ok=True)
    if experiments == ["E1", "E6"]:
        name = "matrix-%s.json" % matrix["date"]
    else:
        name = "matrix-%s-%s.json" % (
            "-".join(e.lower() for e in experiments), matrix["date"])
    out_path = os.path.join(args.out, name)
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(matrix, f, indent=2, sort_keys=True)
        f.write("\n")
    print(summarize(matrix))
    print("wrote %s" % out_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
