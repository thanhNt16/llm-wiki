#!/usr/bin/env python3
"""E2E harness — drive installed agent runtimes through scripted wiki
scenarios and assert on artifacts only (plan Task 12, spec §6 layer 3).

    python3 evals/e2e/run_e2e.py [--runtime omp|claude|cellock|all]
                                [--scenario S1..S6|all] [--timeout N] [--keep]

Per scenario: copy evals/golden-corpus to a temp project, apply the
deterministic setup (none/init+inbox/build/build_uncorrected), run the
runtime CLI headless with the scripted instruction, then run the matching
assert_* from assertions.py — pure filesystem/CLI checks, no LLM judging.

One JSONL row per (runtime, scenario) is appended to results.jsonl:
{runtime, scenario, pass, status, commands_used, tokens_estimate, detail, ...}
status is pass|fail|error|skipped. Absent runtimes produce an explicit
skipped row with a reason. Exit code is 1 on any fail or error row
(a runtime crash/timeout counts — it executed and failed), 0 otherwise.
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

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO_ROOT = os.path.abspath(os.path.join(_HERE, "..", ".."))
sys.path.insert(0, _HERE)
sys.path.insert(0, os.path.join(_REPO_ROOT, "evals", "benchmarks"))

import assertions  # noqa: E402
from driver import Engine, _stage_and_reconcile  # noqa: E402
from fixture import BATCHES, CORPUS_DOCS  # noqa: E402

CORPUS = os.path.join(_REPO_ROOT, "evals", "golden-corpus")
RESULTS = os.path.join(_HERE, "results.jsonl")
RUNS_DIR = os.path.join(_HERE, "runs")
SCENARIOS_PATH = os.path.join(_HERE, "scenarios.json")

RUNTIMES = ("omp", "claude", "cellock")
SCENARIO_IDS = ("S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8")

# When the installed-skill doc root can be resolved it is handed to the agent
# for command references; otherwise the repo copy is used.
_SKILL_DOCS_CANDIDATES = [
    os.path.expanduser("~/.omp/agent/skills/llm-wiki/references/COMMANDS.md"),
    os.path.expanduser("~/.agents/skills/llm-wiki/references/COMMANDS.md"),
    os.path.join(_REPO_ROOT, "skill", "llm-wiki", "references", "COMMANDS.md"),
]

SEED_PAGE = """---
type: concept
title: Retry Policy
subject: retry policy
created: 2026-09-15T00:00:00Z
---

Payment gateway calls retry with exponential backoff; ADR-007 documents 3
attempts while the production matrix shows 5. See [[{claim}]].
"""

INBOX_FILES = {
    "session-2026-10-01.md":
        "# Session 2026-10-01\n\nAgent session notes: investigated retry "
        "flakiness on the payment gateway; concluded the 5-attempt prod "
        "override is intentional pending ADR-008.\n",
    "notes-brainstorm.md":
        "# Brainstorm\n\nLoose notes on attribution dashboards — not a "
        "session log, classify as note.\n",
    "scratch.log":
        "2026-10-01T10:02:11Z INFO worker started\n"
        "2026-10-01T10:02:12Z ERROR retry budget exhausted\n",
}


def _skill_docs():
    for cand in _SKILL_DOCS_CANDIDATES:
        if os.path.isfile(cand):
            return cand
    return ""


def _runtime_command(runtime, instruction, project):
    """Headless invocation per runtime. Returns (argv, cwd) or None if the
    runtime has no usable CLI."""
    if runtime == "omp":
        return (["omp", "-p", "--no-session", "--mode", "json",
                 "--cwd", project, instruction], project)
    if runtime == "claude":
        return (["claude", "-p", "--output-format", "stream-json",
                 "--verbose", "--dangerously-skip-permissions",
                 instruction], project)
    if runtime == "cellock":
        # Probe-only: a headless `cellock` CLI is not known to exist; only
        # ~/.cellockai presence is documented. If a binary ever lands on
        # PATH, drive it print-style like the others.
        return (["cellock", "-p", instruction], project)
    return None


def runtime_skip_reason(runtime):
    """None when the runtime is usable, else the skip reason string."""
    if runtime == "cellock":
        if shutil.which("cellock") or shutil.which("cellockai"):
            return None
        has_dir = os.path.isdir(os.path.expanduser("~/.cellockai"))
        return ("no cellock CLI on PATH (%s present but exposes no headless "
                "entry point)" % "~/.cellockai" if has_dir
                else "no cellock CLI on PATH and no ~/.cellockai")
    if shutil.which(runtime) is None:
        return "%s not on PATH" % runtime
    return None


# --------------------------------------------------------------------- setup

def _init(engine, project):
    code, out, err = engine.run(["init", "--name", "e2e"], cwd=project)
    if code != 0:
        raise RuntimeError("init exit=%d: %s" % (code, err.strip()[:200]))


def _ingest(engine, project, rel):
    code, out, err = engine.run(
        ["ingest", "--file", os.path.join(project, rel)], cwd=project)
    if code != 0:
        raise RuntimeError("ingest %s exit=%d: %s"
                           % (rel, code, err.strip()[:200]))
    try:
        return json.loads(out)["source_id"]
    except ValueError:
        raise RuntimeError("ingest %s: non-JSON stdout" % rel)


def _stage_unrelated(engine, project, src_ids, include_deferred=False):
    """Pass 1 of driver.build_wiki: stage every non-deferred claim UNRELATED.
    include_deferred=False skips the memo's correction claim (left for the
    agent in S4); claim AUTO: supersede markers stay deferred either way."""
    steps = []

    def step(name, args, ok_codes=(0,)):
        code, out, err = engine.run(args, project)
        steps.append({"step": name, "exit": code,
                      "err": err.strip()[-200:] if code not in ok_codes else ""})
        return code, out

    supersede_targets = {}
    for batch in BATCHES:
        rel = batch["source_rel"]
        sid = src_ids[rel]
        cands = []
        for c in batch["claims"]:
            c = json.loads(json.dumps(c))
            if c.pop("defer", None) is not None and not include_deferred:
                continue
            for ev in c["evidence"]:
                ev["source_id"] = sid
            autos = [s for s in c["supersedes"] if s.startswith("AUTO:")]
            c["supersedes"] = [s for s in c["supersedes"]
                               if not s.startswith("AUTO:")]
            if autos:
                supersede_targets.setdefault(
                    autos[0][len("AUTO:"):], []).append((c, len(cands)))
            cands.append(c)
        if not cands:
            continue
        cls = []
        for i, c in enumerate(cands):
            key = "%s|%s|%s" % (c["subject"], c["predicate"],
                                str(c["value"]))
            if key in supersede_targets:
                continue  # claimed as a supersede target by an earlier batch
            cls.append({"index": i, "relationship": "UNRELATED",
                        "target_claim_id": None, "valid_from": None,
                        "valid_to": None,
                        "authority": {"type": c["authority"]["type"]}})
        if not cls:
            continue
        if not _stage_and_reconcile(engine, project, steps, step, sid,
                                    rel, cands, cls):
            raise RuntimeError("stage/reconcile %s failed: %s" % (rel, steps))


def _build_pages_and_verify(engine, project):
    for args in (["build-pages"], ["verify"]):
        code, out, err = engine.run(list(args), cwd=project)
        if code != 0:
            raise RuntimeError("%s exit=%d: %s"
                               % (args[0], code, (out + err).strip()[:200]))
        if args[0] == "verify":
            try:
                if not json.loads(out).get("ok"):
                    raise RuntimeError("verify reported not-ok")
            except ValueError:
                raise RuntimeError("verify non-JSON stdout")


def _seed_retry_page(engine, project):
    """Write wiki/concepts/retry-policy.md so S2's near-dup guard has a twin."""
    claim_id = None
    for path in sorted(glob.glob(os.path.join(
            project, ".llm-wiki", "claims", "claim_*.json"))):
        try:
            with open(path) as f:
                c = json.load(f)
        except (OSError, ValueError):
            continue
        if c.get("subject") == "retry":
            claim_id = c["id"]
            break
    if claim_id is None:
        raise RuntimeError("seed page: no retry-subject claim to dep on")
    code, out, err = engine.run(
        ["write-page", "--artifact", "wiki/concepts/retry-policy.md",
         "--text", SEED_PAGE.replace("{claim}", claim_id)],
        cwd=project)
    if code != 0:
        raise RuntimeError("seed retry-policy page exit=%d: %s"
                           % (code, (out + err).strip()[:300]))


def setup_project(mode, engine, project):
    """Deterministic pre-state per scenario. Never invokes a runtime."""
    if mode == "none":
        return
    if mode in ("init", "init+inbox"):
        _init(engine, project)
        if mode == "init+inbox":
            inbox = os.path.join(project, "inbox")
            os.makedirs(inbox, exist_ok=True)
            for name, body in INBOX_FILES.items():
                with open(os.path.join(inbox, name), "w") as f:
                    f.write(body)
        return
    if mode in ("build", "build_uncorrected"):
        _init(engine, project)
        src_ids = {rel: _ingest(engine, project, rel) for rel in CORPUS_DOCS}
        _stage_unrelated(engine, project, src_ids)
        # build_uncorrected intentionally skips driver's pass-2 correction:
        # the memo source is ingested but its 7d claim is never staged, which
        # is exactly the pending-correction state S4 hands to the agent.
        _build_pages_and_verify(engine, project)
        if mode == "build":
            _seed_retry_page(engine, project)
        return
    raise RuntimeError("unknown setup mode %r" % mode)


# ----------------------------------------------------------------------- run

def run_scenario(runtime, sid, spec, engine, timeout, keep):
    """Execute one (runtime, scenario) pair. Returns the result row."""
    project = tempfile.mkdtemp(prefix="e2e-%s-%s-" % (runtime, sid.lower()))
    transcript = ""
    try:
        shutil.copytree(CORPUS, project, dirs_exist_ok=True)
        setup_project(spec.get("setup", "none"), engine, project)
        instruction = spec["instruction"].format(
            project=project, wiki=engine.wiki_py, docs=_skill_docs())
        argv, cwd = _runtime_command(runtime, instruction, project)

        ts = time.strftime("%Y%m%d-%H%M%S")
        os.makedirs(RUNS_DIR, exist_ok=True)
        transcript = os.path.join(
            RUNS_DIR, "%s-%s-%s.ndjson" % (ts, runtime, sid.lower()))
        try:
            with open(transcript, "w") as f:
                proc = subprocess.run(argv, cwd=cwd, stdout=f,
                                      stderr=subprocess.STDOUT, text=True,
                                      timeout=timeout)
            run_exit = proc.returncode
        except subprocess.TimeoutExpired:
            run_exit = 124

        fn = assertions.ASSERTIONS[spec["assert"]]
        verdict = fn(project, transcript)
        commands = assertions.shell_commands(transcript)
        tokens = assertions.token_estimate(transcript)
        if run_exit != 0:
            # runtime itself failed (crash/timeout/auth) — not an assertion
            # verdict; recorded as error so readers can distinguish.
            status = "error"
        else:
            status = "pass" if verdict["pass"] else "fail"
        row = {
            "ts": ts, "runtime": runtime, "scenario": sid,
            "pass": bool(verdict["pass"]) and run_exit == 0,
            "status": status,
            "assert_pass": bool(verdict["pass"]),
            "runtime_exit": run_exit,
            "commands_used": commands,
            "tokens_estimate": tokens,
            "detail": verdict["detail"],
            "transcript": transcript,
        }
        if run_exit != 0:
            row["detail"] += " (runtime exit=%d)" % run_exit
        return row
    except Exception as e:  # setup/instruction bugs → executed error row
        return {"ts": time.strftime("%Y%m%d-%H%M%S"), "runtime": runtime,
                "scenario": sid, "pass": False, "status": "error",
                "commands_used": [], "tokens_estimate": 0,
                "detail": "harness error: %s" % e,
                "transcript": transcript}
    finally:
        if not keep:
            shutil.rmtree(project, ignore_errors=True)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--runtime", default="all",
                    choices=list(RUNTIMES) + ["all"])
    ap.add_argument("--scenario", default="all",
                    choices=list(SCENARIO_IDS) + ["all"])
    ap.add_argument("--timeout", type=int, default=900,
                    help="per-scenario runtime timeout seconds")
    ap.add_argument("--keep", action="store_true",
                    help="keep temp project dirs for debugging")
    args = ap.parse_args(argv)

    with open(SCENARIOS_PATH) as f:
        scenarios = json.load(f)
    engine = Engine(assertions.engine_path())
    if not engine.wiki_py:
        print("error: no wiki.py engine found "
              "(set WIKI_E2E_ENGINE)", file=sys.stderr)
        return 2

    runtimes = RUNTIMES if args.runtime == "all" else (args.runtime,)
    sids = SCENARIO_IDS if args.scenario == "all" else (args.scenario,)

    rows = []
    for runtime in runtimes:
        reason = runtime_skip_reason(runtime)
        for sid in sids:
            if reason:
                row = {"ts": time.strftime("%Y%m%d-%H%M%S"),
                       "runtime": runtime, "scenario": sid, "pass": None,
                       "status": "skipped", "commands_used": [],
                       "tokens_estimate": 0, "detail": reason}
            else:
                row = run_scenario(runtime, sid, scenarios[sid], engine,
                                   args.timeout, args.keep)
            rows.append(row)
            print("%-7s %-2s %-7s %s"
                  % (runtime, sid, row["status"], row["detail"][:140]))

    with open(RESULTS, "a") as f:
        for row in rows:
            f.write(json.dumps(row) + "\n")

    failed = [r for r in rows if r["status"] in ("fail", "error")]
    print("%d row(s) appended to %s; %d skipped, %d passed, %d failed"
          % (len(rows), RESULTS,
             sum(1 for r in rows if r["status"] == "skipped"),
             sum(1 for r in rows if r["status"] == "pass"),
             len(failed)))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
