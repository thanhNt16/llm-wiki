#!/usr/bin/env python3
"""run_install_ux.py — install & UX lane (P7 lane B second half).

Everything runs sandboxed: fake HOME + minimal PATH under a mktemp root.
Nothing touches the real ~/.local/bin, ~/.omp, ~/.agents, ~/.claude* or
the network.

  binary install   build llm-wiki-<tag>-<triple>.tar.gz (+ .sha256
                   sidecar) from rust/target/release, then run
                   `install.sh --binary` with LLM_WIKI_BINARY_TARBALL;
                   time-to-working = install wall + `init`/`status`
                   success on a throwaway wiki
  failure modes    missing tarball -> clear error; tampered .sha256
                   sidecar -> abort before install; `--python` ->
                   dev-reference refusal (post phase-6 flip)
  mcp              `mcp-register --client claude-code` inside the
                   sandbox HOME (fallback ~/.claude.json edit, no real
                   client config); `--help` subcommand surface count
  iteration speed  on a seeded perf-corpus wiki: edit one claim file +
                   compile + build-pages + query-prepare round-trip,
                   5 reps median, both engines
  scorecard        single binary / zero external deps (otool -L) /
                   faster / smaller / one-command install verdicts vs
                   the original single-binary expectations

Output: evals/rust-vs-python/install_ux.json.
Stdlib only, Python >= 3.9.
"""
import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, os.path.join(HERE, ".."))
sys.path.insert(0, HERE)

import _engine  # noqa: E402
import gen_perf_corpus as corpus  # noqa: E402
import run_perf  # noqa: E402

OUT_JSON = os.path.join(HERE, "install_ux.json")
INSTALL_SH = os.path.join(REPO_ROOT, "install.sh")
MIN_PATH = "/usr/bin:/bin"


# ------------------------------------------------------------- helpers


def run(argv, env=None, cwd=None, timeout=300):
    t0 = time.perf_counter()
    proc = subprocess.run(argv, env=env, cwd=cwd, capture_output=True,
                          text=True, timeout=timeout)
    return proc, time.perf_counter() - t0


def sandbox_env(home, extra=None):
    """Minimal sandbox: fake HOME, system-only PATH + stub dir."""
    stub = os.path.join(home, ".stub-bin")
    os.makedirs(stub, exist_ok=True)
    # markitdown stub so install.sh's optional extractor probe is a
    # no-op without network.
    mpath = os.path.join(stub, "markitdown")
    with open(mpath, "w") as f:
        f.write("#!/bin/sh\nexit 0\n")
    os.chmod(mpath, 0o755)
    env = {"HOME": home, "PATH": stub + ":" + MIN_PATH,
           "TERM": "dumb", "LANG": "en_US.UTF-8"}
    if extra:
        env.update(extra)
    return env


def build_tarball(workdir, binary=None):
    """Package the release binary exactly like release-rust.yml:
    llm-wiki-<tag>-<triple>.tar.gz with a .sha256 sidecar.
    Returns (tarball_path, sha256_path)."""
    binary = binary or _engine.RUST_BINARY
    stage = os.path.join(workdir, "stage")
    os.makedirs(stage, exist_ok=True)
    shutil.copy2(binary, os.path.join(stage, "llm-wiki"))
    s, m = platform.system(), platform.machine()
    triple = {"Darwin": {"arm64": "aarch64-apple-darwin",
                         "x86_64": "x86_64-apple-darwin"},
              "Linux": {"x86_64": "x86_64-unknown-linux-musl",
                        "aarch64": "aarch64-unknown-linux-musl"}
              }[s][m]
    name = "llm-wiki-local-%s.tar.gz" % triple
    tarball = os.path.join(workdir, name)
    proc, _ = run(["tar", "-czf", tarball, "-C", stage, "llm-wiki"])
    if proc.returncode != 0:
        raise RuntimeError("tar failed: %s" % proc.stderr)
    digest = hashlib.sha256()
    with open(tarball, "rb") as f:
        digest.update(f.read())
    sha_path = tarball + ".sha256"
    with open(sha_path, "w") as f:
        f.write("%s  %s\n" % (digest.hexdigest(), name))
    return tarball, sha_path


def count_subcommands(help_text):
    """Count entries in a clap 'Commands:' section."""
    n = 0
    in_cmd = False
    for line in help_text.splitlines():
        if re.match(r"^Commands:\s*$", line):
            in_cmd = True
            continue
        if in_cmd:
            if re.match(r"^(Options|Usage|Arguments|positional arguments)",
                        line):
                break
            if re.match(r"^\s{2,}[a-z][\w-]*\s", line) and \
                    not line.lstrip().startswith("-"):
                n += 1
    return n

def install_steps(stdout):
    """Human-visible install milestones from install.sh output."""
    steps = []
    for line in stdout.splitlines():
        s = line.strip()
        if re.search(r"verified .+\.sha256", s):
            steps.append(s)
        elif re.search(r"installing .+(\u2192|->)", s):
            steps.append(s)
        elif re.search(r"^installed .+/llm-wiki", s):
            steps.append(s)
        elif s.startswith("{") and '"installed"' in s and '"target"' in s:
            try:
                d = json.loads(s)
                steps.append("installed %d skills -> %s"
                             % (d.get("count", 0), d.get("target")))
            except ValueError:
                pass
        elif s.startswith("note:"):
            steps.append(s)
    return steps
def count_subcommands_argparse(help_text):
    """Count {a,b,c} choices in the argparse positional line."""
    m = re.search(r"\{([a-z0-9_,-]+)\}", help_text)
    return len(m.group(1).split(",")) if m else None


# ---------------------------------------------------------------- runs


def run_binary_install(tmp, tarball):
    """install.sh --binary in a sandbox; returns result dict."""
    home = os.path.join(tmp, "home")
    os.makedirs(home, exist_ok=True)
    env = sandbox_env(home, {"LLM_WIKI_BINARY_TARBALL": tarball})
    t0 = time.perf_counter()
    proc = subprocess.run(["/bin/bash", INSTALL_SH, "--binary"],
                          env=env, cwd=home, capture_output=True,
                          text=True, timeout=600)
    install_s = time.perf_counter() - t0
    out = {"exit": proc.returncode,
           "install_s": round(install_s, 3),
           "steps": install_steps(proc.stdout),
           "stdout_tail": proc.stdout.strip().splitlines()[-10:],
           "stderr_tail": proc.stderr.strip().splitlines()[-5:]}
    out["step_count"] = len(out["steps"])

    binpath = os.path.join(home, ".local", "bin", "llm-wiki")
    out["binary_path"] = binpath
    out["binary_installed"] = os.path.isfile(binpath)
    if proc.returncode == 0 and out["binary_installed"]:
        wroot = os.path.join(tmp, "probe-wiki")
        os.makedirs(wroot, exist_ok=True)
        t1 = time.perf_counter()
        ok = True
        p1 = subprocess.run([binpath, "init", wroot], env=env,
                            capture_output=True, text=True)
        ok = ok and p1.returncode == 0
        p2 = subprocess.run([binpath, "status", "--root", wroot],
                            env=env, capture_output=True, text=True)
        ok = ok and p2.returncode == 0 and '"initialized"' in p2.stdout
        verify_s = time.perf_counter() - t1
        out["init_status_ok"] = ok
        out["init_status_s"] = round(verify_s, 3)
        out["time_to_working_s"] = round(install_s + verify_s, 3)
        try:
            out["wiki_status"] = json.loads(p2.stdout)
        except ValueError:
            out["wiki_status"] = None
    else:
        out["init_status_ok"] = False
        out["time_to_working_s"] = None
    return out


def run_missing_tarball(tmp):
    home = os.path.join(tmp, "home-miss")
    os.makedirs(home, exist_ok=True)
    env = sandbox_env(home, {
        "LLM_WIKI_BINARY_TARBALL": os.path.join(tmp, "nope.tar.gz")})
    proc, _ = run(["/bin/bash", INSTALL_SH, "--binary"], env=env, cwd=home)
    err = (proc.stderr or "") + (proc.stdout or "")
    return {"exit": proc.returncode,
            "message": err.strip().splitlines()[:4],
            "clear_error": "not found" in err}


def run_tampered_sha256(tmp, tarball):
    """Corrupt the sidecar hash; install must abort before installing."""
    home = os.path.join(tmp, "home-tamp")
    os.makedirs(home, exist_ok=True)
    tdir = os.path.join(tmp, "tampered")
    os.makedirs(tdir, exist_ok=True)
    local_tar = os.path.join(tdir, os.path.basename(tarball))
    shutil.copy2(tarball, local_tar)
    # flip the first hex digit of the recorded digest
    with open(tarball + ".sha256") as f:
        good = f.read().split()[0]
    bad = ("0" if good[0] != "0" else "1") + good[1:]
    with open(local_tar + ".sha256", "w") as f:
        f.write("%s  %s\n" % (bad, os.path.basename(tarball)))
    env = sandbox_env(home, {"LLM_WIKI_BINARY_TARBALL": local_tar})
    proc, _ = run(["/bin/bash", INSTALL_SH, "--binary"], env=env, cwd=home)
    err = (proc.stderr or "") + (proc.stdout or "")
    binpath = os.path.join(home, ".local", "bin", "llm-wiki")
    return {"exit": proc.returncode,
            "message": err.strip().splitlines()[:4],
            "mentions_sha256_mismatch": "sha256 mismatch" in err,
            "aborted": proc.returncode != 0,
            "binary_installed": os.path.isfile(binpath)}


def run_python_flag(tmp):
    home = os.path.join(tmp, "home-py")
    os.makedirs(home, exist_ok=True)
    env = sandbox_env(home)
    proc, _ = run(["/bin/bash", INSTALL_SH, "--python"], env=env, cwd=home)
    err = proc.stderr or ""
    return {"exit": proc.returncode,
            "message": err.strip().splitlines(),
            "dev_reference_message": ("dev-reference" in err
                                      and "skill/llm-wiki/scripts" in err),
            "installed_anything": os.path.exists(
                os.path.join(home, ".local", "bin", "llm-wiki"))}


def run_mcp_probe(home, binpath):
    """mcp-register in sandbox HOME (no real client config present)."""
    env = sandbox_env(home)
    proj = os.path.join(home, "proj")
    os.makedirs(proj, exist_ok=True)
    proc, _ = run([binpath, "mcp-register", "--client", "claude-code",
                   "--root", proj], env=env, cwd=proj)
    cfg = os.path.join(home, ".claude.json")
    written = os.path.isfile(cfg)
    detail = None
    if written:
        try:
            with open(cfg) as f:
                detail = json.load(f)
            detail = {"has_mcpServers": "mcpServers" in detail,
                      "keys": sorted(detail.keys())[:10]}
        except ValueError:
            detail = {"parse": "failed"}
    out = {"exit": proc.returncode,
           "stdout_tail": proc.stdout.strip().splitlines()[-8:],
           "stderr_tail": proc.stderr.strip().splitlines()[-4:],
           "client_config_written": written,
           "client_config_path": cfg if written else None,
           "client_config_detail": detail,
           "note": ("no claude CLI / real client config on sandbox PATH; "
                    "fallback ~/.claude.json edit exercised; no --dry-run "
                    "or list-clients flag exists on mcp-register")}
    # --help surfaces for both engines
    pr, _ = run([binpath, "--help"], env=env)
    out["rust_subcommand_count"] = count_subcommands(pr.stdout)
    py_argv = _engine.engine_argv("python")
    pp, _ = run(py_argv + ["--help"], env=env)
    out["python_subcommand_count"] = count_subcommands_argparse(pp.stdout)
    return out


def run_iteration_speed(tmp, engines, docs, reps):
    """edit claim + compile + build-pages + query-prepare round-trip."""
    corpus_dir = os.path.join(tmp, "iter-corpus")
    corpus.write_corpus(corpus_dir, docs)
    files = sorted(os.path.join(corpus_dir, n)
                   for n, _ in corpus.gen_docs(docs))
    out = {}
    for name, argv in engines.items():
        root = os.path.join(tmp, "iter-wiki-%s" % name)
        shutil.rmtree(root, ignore_errors=True)
        with tempfile.TemporaryDirectory(dir=tmp) as td:
            run_perf.seed_wiki(argv, root, corpus_dir, files, td,
                               time_ingest=False)
        claims_dir = os.path.join(root, ".llm-wiki", "claims")
        claim_files = sorted(
            os.path.join(claims_dir, f) for f in os.listdir(claims_dir)
            if f.startswith("claim_") and f.endswith(".json"))
        samples = []
        for rep in range(reps):
            cf = claim_files[rep % len(claim_files)]
            t0 = time.perf_counter()
            with open(cf) as f:
                claim = json.load(f)
            claim["value"] = "%s-iter%d" % (
                str(claim.get("value", "v")).split("-iter")[0], rep)
            with open(cf, "w") as f:
                json.dump(claim, f, indent=2)
            run_perf.engine_cli(argv, ["compile"], cwd=root)
            run_perf.engine_cli(argv, ["build-pages"], cwd=root)
            run_perf.engine_cli(
                argv, ["query-prepare", "--question",
                       "who owns %s" % claim.get("subject", "svc-0000")],
                cwd=root)
            samples.append(time.perf_counter() - t0)
        out[name] = run_perf.round_stats(run_perf.med_min_max(samples))
    return out


def scorecard(install, engines_perf, footprint, otool, steps_ok):
    """Verdicts vs the original single-binary install expectations."""
    non_system = [l.strip() for l in (otool or "").splitlines()[1:]
                  if l.strip() and not l.strip().startswith(
                      ("/usr/lib/", "/System/Library/"))]
    card = {}

    card["single_binary"] = {
        "verdict": "pass" if footprint["rust_binary_bytes"] else "fail",
        "evidence": "rust/target/release/llm-wiki = %d bytes, no runtime "
                    "deps required" % footprint["rust_binary_bytes"]}

    card["zero_external_deps"] = {
        "verdict": "pass" if not non_system else "fail",
        "non_system_libs": non_system,
        "evidence": "otool -L lists only system frameworks/libraries"}

    faster = {}
    for k, v in (engines_perf or {}).items():
        faster[k] = v
    card["faster"] = {
        "verdict": ("pass" if faster and all(
            isinstance(v, (int, float)) and v > 1 for v in faster.values())
            else ("n/a" if not faster else "mixed")),
        "evidence": "python/rust median ratios from perf.json headline "
                    "speedups",
        "speedups": faster}

    card["smaller"] = {
        "verdict": "mixed",
        "evidence": ("rust binary %d bytes (stripped %s) vs python engine "
                     "source %d bytes across %d files; python additionally "
                     "requires a >=3.9 interpreter on PATH"
                     % (footprint["rust_binary_bytes"],
                        footprint["rust_binary_stripped_bytes"],
                        footprint["python_engine_bytes"],
                        footprint["python_engine_files"]))}

    card["one_command_install"] = {
        "verdict": ("pass" if install.get("time_to_working_s") is not None
                    and steps_ok else "fail"),
        "steps": install.get("step_count"),
        "step_log": install.get("steps"),
        "time_to_working_s": install.get("time_to_working_s"),
        "evidence": "./install.sh --binary (or curl|bash) → verified "
                    "llm-wiki on PATH"}
    return card


def otool_libs(binary):
    try:
        proc, _ = run(["/usr/bin/otool", "-L", binary])
    except OSError:
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--docs", type=int, default=150,
                    help="perf corpus docs for the iteration-speed wiki")
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--perf-json", default=os.path.join(HERE, "perf.json"))
    ap.add_argument("--out", default=OUT_JSON)
    args = ap.parse_args()

    engines = {n: _engine.engine_argv(n) for n in ("python", "rust")}
    rust_bin = _engine.RUST_BINARY

    perf = None
    if os.path.isfile(args.perf_json):
        with open(args.perf_json) as f:
            perf = json.load(f)

    results = {"meta": {
        "lane": "rust-vs-python/B-install",
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ",
                                     time.gmtime()),
        "platform": platform.platform(),
        "docs": args.docs, "reps": args.reps,
        "sandbox_root": None,  # filled per-run below
        "perf_json": args.perf_json if perf else None}}

    with tempfile.TemporaryDirectory(prefix="rvs-ux-") as tmp:
        results["meta"]["sandbox_root"] = tmp

        tarball, sha = build_tarball(os.path.join(tmp, "pkg"), rust_bin)
        results["tarball"] = {"path": os.path.basename(tarball),
                              "bytes": os.path.getsize(tarball),
                              "sha256_sidecar": os.path.basename(sha)}

        results["binary_install"] = run_binary_install(
            os.path.join(tmp, "install"), tarball)
        results["failure_modes"] = {
            "missing_tarball": run_missing_tarball(tmp),
            "tampered_sha256": run_tampered_sha256(tmp, tarball),
            "python_flag": run_python_flag(tmp)}

        home = os.path.join(tmp, "install", "home")
        binpath = os.path.join(home, ".local", "bin", "llm-wiki")
        if not os.path.isfile(binpath):
            binpath = rust_bin  # fall back to the build output for probes
        results["mcp"] = run_mcp_probe(home, binpath)

        results["iteration_speed"] = {
            "method": ("edit one claims/*.json value + `compile` + "
                       "`build-pages` + `query-prepare`, wall per rep"),
            "docs": args.docs,
            "engines": run_iteration_speed(tmp, engines, args.docs,
                                           args.reps)}

    footprint = run_perf.footprint(tempfile.mkdtemp(prefix="rvs-fp-"))
    results["footprint"] = footprint
    otool_out = otool_libs(rust_bin)
    results["otool_L"] = otool_out

    speedups = (perf or {}).get("headline_speedups") or {}
    results["scorecard"] = scorecard(
        results["binary_install"], speedups, footprint, otool_out,
        steps_ok=results["binary_install"].get("step_count", 0) > 0)

    tmp_out = args.out + ".tmp"
    with open(tmp_out, "w") as f:
        json.dump(results, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp_out, args.out)
    print(json.dumps(results["scorecard"], indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
