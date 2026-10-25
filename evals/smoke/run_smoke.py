#!/usr/bin/env python3
"""Install-to-first-query smoke harness, one pass per install mode.

Per available mode, via subprocess only:
  install.sh (mode flag) into an isolated fake $HOME
    -> resolve installed skill/llm-wiki/scripts/wiki.py
    -> temp project: init -> ingest golden ADR -> verify ok:true
       -> doctor has no error-severity findings
       -> query-prepare --question "retries" exits 0
    -> install.sh --uninstall -> assert no llm-wiki leftovers in the
       mode's skills dir.

Missing runtime/dir -> mode recorded "skipped" with a reason and the run
still exits 0. A failed *executed* step -> mode "failed", exit 1.
No LLM calls; per-mode wall time is recorded against a < 10 s budget.

Usage:
  python3 evals/smoke/run_smoke.py [--mode omp|agents|cellockai|all]
"""
import argparse
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
INSTALL = os.path.join(REPO_ROOT, "install.sh")
GOLDEN_DOCS = os.path.join(REPO_ROOT, "evals", "golden-corpus", "docs")

MODE_BUDGET_S = 10.0

SKILL_NAMES = ("llm-wiki", "wiki-init", "wiki-ingest", "wiki-compile",
               "wiki-query", "wiki-context", "wiki-review", "wiki-doctor",
               "wiki-visualize", "wiki-write")

# mode -> (install.sh flag, skills dir relative to fake $HOME, gate)
MODES = {
    "omp":       (None,           ".omp/agent/skills",
                  lambda: shutil.which("omp") is not None
                          or os.path.isdir(os.path.expanduser("~/.omp")),
                  "no omp CLI on PATH and no ~/.omp dir"),
    "agents":    ("--agents",     ".agents/skills",
                  lambda: os.path.isdir(os.path.expanduser("~/.agents"))
                          or shutil.which("claude") is not None,
                  "no ~/.agents dir and no claude CLI on PATH"),
    "cellockai": ("--cellockai",  ".cellockai/skills",
                  lambda: (shutil.which("cellock") is not None
                           or shutil.which("cellockai") is not None),
                  "cellock CLI not on PATH"),
}


def _real_dir(binary):
    path = shutil.which(binary)
    return os.path.dirname(path) if path else None


def make_env(home, stubbin):
    """Isolated env: fake $HOME, stubbed optional-extractor installers."""
    path_parts = [stubbin, "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
    for d in (_real_dir("git"), _real_dir("python3"), _real_dir("bash")):
        if d and d not in path_parts:
            path_parts.append(d)
    env = dict(os.environ)
    env["HOME"] = home
    env["PATH"] = ":".join(path_parts)
    return env


def make_stubbin(tmp):
    """markitdown stubbed present so install.sh never reaches uv/pipx/pip3;
    those are stubbed no-ops anyway as belt-and-braces."""
    stubbin = os.path.join(tmp, "stubbin")
    os.makedirs(stubbin, exist_ok=True)
    for name in ("markitdown", "uv", "pipx", "pip3"):
        stub = os.path.join(stubbin, name)
        with open(stub, "w") as f:
            f.write("#!/bin/sh\nexit 0\n")
        os.chmod(stub, stat.S_IRWXU)
    return stubbin


def resolve_fixture():
    exact = os.path.join(GOLDEN_DOCS, "adr-007-retries.md")
    if os.path.isfile(exact):
        return exact
    if os.path.isdir(GOLDEN_DOCS):
        for name in sorted(os.listdir(GOLDEN_DOCS)):
            if name.startswith("adr-") and name.endswith(".md"):
                return os.path.join(GOLDEN_DOCS, name)
    return None


def run_step(rec, name, argv, env, cwd=None, check=None):
    """Run one step; record it; return (ok, detail)."""
    proc = subprocess.run(argv, env=env, cwd=cwd,
                          capture_output=True, text=True, timeout=60)
    detail = ""
    ok = proc.returncode == 0
    if ok and check is not None:
        ok, detail = check(proc)
    if not ok and not detail:
        detail = (proc.stderr or proc.stdout or "").strip().splitlines()
        detail = detail[-1] if detail else "exit %d" % proc.returncode
    rec["steps"].append({"name": name, "ok": ok, "detail": detail,
                         "exit": proc.returncode})
    return ok, proc


def check_json(pred, what):
    def _check(proc):
        try:
            data = json.loads(proc.stdout)
        except ValueError:
            return False, "%s: stdout not JSON" % what
        return (True, "") if pred(data) else (False, "%s: %s" % (what, proc.stdout.strip()[:200]))
    return _check


def leftovers(skills_dir):
    if not os.path.isdir(skills_dir):
        return []
    return [n for n in os.listdir(skills_dir)
            if n in SKILL_NAMES or n.startswith("llm-wiki")]


def smoke_mode(mode, flag, rel_skills, fixture, stubbin):
    rec = {"mode": mode, "status": "failed", "elapsed_s": 0.0,
           "budget_s": MODE_BUDGET_S, "steps": []}
    t0 = time.monotonic()
    tmp = tempfile.mkdtemp(prefix="smoke-%s-" % mode)
    try:
        home = os.path.join(tmp, "home")
        project = os.path.join(tmp, "project")
        os.makedirs(home)
        os.makedirs(project)
        env = make_env(home, stubbin)
        skills_dir = os.path.join(home, rel_skills)

        # 1. install
        argv = ["bash", INSTALL] + ([flag] if flag else [])
        ok, _ = run_step(rec, "install", argv, env)
        if not ok:
            return rec

        # 2. resolve installed engine
        engine = os.path.join(skills_dir, "llm-wiki", "scripts", "wiki.py")
        ok = os.path.isfile(engine)
        rec["steps"].append({"name": "resolve_engine", "ok": ok,
                             "detail": engine, "exit": 0 if ok else 1})
        if not ok:
            return rec

        # 3. engine lifecycle in temp project
        seq = [
            ("init", [sys.executable, engine, "init", "--name", "smoke"],
             check_json(lambda d: d.get("initialized") is True, "init")),
            ("ingest", [sys.executable, engine, "ingest", "--file", fixture],
             check_json(lambda d: bool(d.get("source_id")), "ingest")),
            ("verify", [sys.executable, engine, "verify"],
             check_json(lambda d: d.get("ok") is True, "verify ok")),
            ("doctor", [sys.executable, engine, "doctor"],
             check_json(lambda d: not any(
                 f.get("severity") == "error"
                 for f in d.get("findings", [])), "doctor errors")),
            ("query_prepare",
             [sys.executable, engine, "query-prepare", "--question", "retries"],
             None),
        ]
        for name, argv, check in seq:
            ok, _ = run_step(rec, name, argv, env, cwd=project, check=check)
            if not ok:
                return rec

        # 4. uninstall + leftover assert
        ok, _ = run_step(rec, "uninstall", ["bash", INSTALL, "--uninstall"], env)
        if not ok:
            return rec
        left = leftovers(skills_dir)
        ok = not left
        rec["steps"].append({"name": "no_leftovers", "ok": ok,
                             "detail": ",".join(left), "exit": 0 if ok else 1})
        if not ok:
            return rec

        rec["status"] = "passed"
        return rec
    finally:
        rec["elapsed_s"] = round(time.monotonic() - t0, 3)
        rec["over_budget"] = rec["elapsed_s"] > MODE_BUDGET_S
        shutil.rmtree(tmp, ignore_errors=True)


def main(argv=None):
    ap = argparse.ArgumentParser(description="install-mode smoke harness")
    ap.add_argument("--mode", choices=list(MODES) + ["all"], default="all")
    args = ap.parse_args(argv)

    fixture = resolve_fixture()
    if fixture is None:
        out = {"ok": False, "error": "no ADR fixture under %s" % GOLDEN_DOCS,
               "modes": []}
        print(json.dumps(out, indent=2))
        print("SMOKE FAIL")
        return 1

    stub_tmp = tempfile.mkdtemp(prefix="smoke-stubs-")
    stubbin = make_stubbin(stub_tmp)
    try:
        selected = list(MODES) if args.mode == "all" else [args.mode]
        results = []
        for mode in selected:
            flag, rel_skills, gate, skip_reason = MODES[mode]
            if not gate():
                results.append({"mode": mode, "status": "skipped",
                                "reason": skip_reason, "elapsed_s": 0.0,
                                "budget_s": MODE_BUDGET_S, "steps": []})
                continue
            results.append(
                smoke_mode(mode, flag, rel_skills, fixture, stubbin))
    finally:
        shutil.rmtree(stub_tmp, ignore_errors=True)

    ok = all(r["status"] in ("passed", "skipped") for r in results)
    summary = {"ok": ok,
               "fixture": os.path.relpath(fixture, REPO_ROOT),
               "modes": results}
    print(json.dumps(summary, indent=2))
    print("SMOKE OK" if ok else "SMOKE FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
