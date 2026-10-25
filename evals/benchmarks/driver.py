"""Subprocess wrapper around a wiki.py engine checkout.

The bench harness NEVER imports wikicore from the engine under test — it
drives the CLI so the identical harness measures any engine revision (base
bb7a51f or HEAD) fairly.

Classification shapes (verified against wikicore/compile.py):
- relationship must be one of CORRECTION, POLICY_CHANGE, SCOPE_DIFFERENCE,
  CONTRADICTION, CORROBORATION, DUPLICATE, UNRELATED ("CORRECTS" does not
  exist; the correction relationship is CORRECTION and requires
  target_claim_id).
- authority.type must satisfy the claim-schema enum; "authoritative_source"
  is valid and auto-accepted under the default acceptance policy.
- on CORRECTION the engine appends target_claim_id to the new claim's
  supersedes itself (claim.schema.json requires uniqueItems), so the staged
  correction candidate carries an empty supersedes list.
"""
import glob
import json
import os
import subprocess
import sys
import tempfile

# Import both ways so the module works package-style
# (`import evals.benchmarks.driver`) and with evals/benchmarks on sys.path
# (`from driver import ...`).
try:
    from .fixture import BATCHES, CORRECTION_SOURCE_REL, CORPUS_DOCS
except ImportError:
    from fixture import BATCHES, CORRECTION_SOURCE_REL, CORPUS_DOCS


class Engine:
    def __init__(self, wiki_py: str):
        # absolutize: subprocesses run with cwd=<wiki root>, not this process's cwd
        self.wiki_py = os.path.abspath(wiki_py)

    def run(self, args, cwd=None, input_text=None):
        try:
            proc = subprocess.run(
                [sys.executable, self.wiki_py] + [str(a) for a in args],
                cwd=str(cwd or os.getcwd()), capture_output=True, text=True,
                input=input_text, timeout=300)
        except subprocess.TimeoutExpired:
            return 124, "", "timeout"
        return proc.returncode, proc.stdout, proc.stderr

    def json(self, args, cwd=None, input_text=None):
        code, out, err = self.run(args, cwd, input_text)
        if code != 0:
            return code, None, err
        try:
            return code, json.loads(out), err
        except ValueError:
            return code, None, "non-JSON stdout: %s" % out[:200]


def _short_value(v):
    return str(v)


def _stage_and_reconcile(engine, wiki_root, steps, step, sid, rel, cands, cls):
    """stage-candidates + reconcile-apply one batch; True iff both exit 0.

    Temp files are tracked and unlinked in a finally so nothing leaks when a
    step fails or the run_id parse degrades to a recorded step error."""
    tmp_paths = []
    try:
        with tempfile.NamedTemporaryFile("w", suffix=".json",
                                         delete=False) as f:
            json.dump(cands, f)
            cand_path = f.name
        tmp_paths.append(cand_path)
        report = {"source_id": sid, "source_version": 1,
                  "coverage": {"text": "complete"}, "warnings": [],
                  "parser": {"name": "bench", "version": "1"}}
        with tempfile.NamedTemporaryFile("w", suffix=".json",
                                         delete=False) as f:
            json.dump(report, f)
            report_path = f.name
        tmp_paths.append(report_path)
        code, out = step("stage " + rel,
                         ["stage-candidates", "--file", cand_path,
                          "--source-id", sid, "--source-version", "1",
                          "--report", report_path])
        if code != 0:
            return False
        try:
            run_id = json.loads(out).get("run_id")
        except ValueError:
            steps.append({"step": "parse run_id " + rel, "exit": 1,
                          "err": "non-JSON stdout from stage-candidates"})
            return False
        with tempfile.NamedTemporaryFile("w", suffix=".json",
                                         delete=False) as f:
            json.dump(cls, f)
            cls_path = f.name
        tmp_paths.append(cls_path)
        code, _ = step("reconcile " + rel,
                       ["reconcile-apply", "--run", run_id,
                        "--classifications", cls_path])
        return code == 0
    finally:
        for p in tmp_paths:
            try:
                os.unlink(p)
            except OSError:
                pass


def _apply_correction(engine, wiki_root, src_ids, steps=None):
    """Pass 2: the deferred memo correction (attribution window 30 -> 7).

    Finds the 30d claim created in pass 1 (glob .llm-wiki/claims/), re-stages
    the memo's window-days candidate alone, and reconciles it CORRECTION
    against that claim. The candidate is staged with empty supersedes —
    compile.py appends the target id itself and claim.schema.json requires
    uniqueItems.
    """
    if steps is None:
        steps = []

    def step(name, args, ok_codes=(0,)):
        code, out, err = engine.run(args, wiki_root)
        steps.append({"step": name, "exit": code,
                      "err": err.strip()[-200:] if code not in ok_codes else ""})
        return code, out

    sid = src_ids[CORRECTION_SOURCE_REL]

    target_id = None
    for path in sorted(glob.glob(os.path.join(
            wiki_root, ".llm-wiki", "claims", "claim_*.json"))):
        with open(path) as f:
            c = json.load(f)
        if (c.get("subject") == "attribution"
                and c.get("predicate") == "window-days"
                and c.get("value") == 30):
            target_id = c["id"]
            break
    if target_id is None:
        steps.append({"step": "correction lookup", "exit": 3,
                      "err": "no attribution/window-days=30 claim found"})
        return False

    memo_batch = next(b for b in BATCHES if b["source_rel"] == CORRECTION_SOURCE_REL)
    cand = next(c for c in memo_batch["claims"] if c.get("defer") == "correction")
    cand = json.loads(json.dumps(cand))  # deep copy; never mutate the fixture
    cand.pop("defer", None)
    for ev in cand["evidence"]:
        ev["source_id"] = sid
    cand["supersedes"] = []  # engine appends target_id on CORRECTION

    cls = [{"index": 0, "relationship": "CORRECTION",
            "target_claim_id": target_id,
            "valid_from": "2026-09-01", "valid_to": None,
            "authority": {"type": cand["authority"]["type"]}}]
    return _stage_and_reconcile(engine, wiki_root, steps, step, sid,
                                CORRECTION_SOURCE_REL + " correction",
                                [cand], cls)


def build_wiki(engine: Engine, corpus_dir: str, wiki_root: str) -> dict:
    """init -> ingest corpus docs -> stage planted claims -> reconcile ->
    correction pass -> build-pages -> verify.
    Returns {"steps": [{step, exit}...], "ok": bool, "source_ids": {...}}."""
    corpus_dir = os.path.abspath(corpus_dir)  # subprocess cwd is wiki_root
    steps = []

    def step(name, args, ok_codes=(0,)):
        code, out, err = engine.run(args, wiki_root)
        steps.append({"step": name, "exit": code,
                      "err": err.strip()[-200:] if code not in ok_codes else ""})
        return code, out

    code, _ = step("init", ["init", "--name", "bench"])
    if code != 0:
        return {"steps": steps, "ok": False}

    src_ids = {}
    for rel in CORPUS_DOCS:
        code, out = step("ingest " + rel,
                         ["ingest", "--file", os.path.join(corpus_dir, rel)])
        if code != 0:
            return {"steps": steps, "ok": False}
        try:
            src_ids[rel] = json.loads(out)["source_id"]
        except ValueError:
            steps.append({"step": "parse ingest " + rel, "exit": 1,
                          "err": "non-JSON stdout from ingest"})
            return {"steps": steps, "ok": False}

    # pass 1: stage + reconcile every claim UNRELATED, except claims marked
    # "defer" in the fixture (the memo's correction) — stripped below so the
    # engine never sees the key.
    supersede_targets = {}
    for batch in BATCHES:
        rel = batch["source_rel"]
        sid = src_ids[rel]
        cands = []
        for c in batch["claims"]:
            c = json.loads(json.dumps(c))
            if c.pop("defer", None) is not None:
                continue  # deferred to pass 2 (_apply_correction)
            for ev in c["evidence"]:
                ev["source_id"] = sid
            sup = [s for s in c["supersedes"] if not s.startswith("AUTO:")]
            autos = [s for s in c["supersedes"] if s.startswith("AUTO:")]
            if autos:
                key = autos[0][len("AUTO:"):]
                supersede_targets.setdefault(key, []).append((c, len(cands)))
            c["supersedes"] = sup
            cands.append(c)
        if not cands:
            continue  # fully deferred batch (the memo) — pass 2 handles it
        cls = []
        for i, c in enumerate(cands):
            key = "%s|%s|%s" % (c["subject"], c["predicate"],
                                _short_value(c["value"]))
            if key in supersede_targets:
                continue  # claimed as a supersede target by an earlier batch
            cls.append({"index": i, "relationship": "UNRELATED",
                        "target_claim_id": None, "valid_from": None,
                        "valid_to": None,
                        "authority": {"type": c["authority"]["type"]}})
        if not _stage_and_reconcile(engine, wiki_root, steps, step, sid,
                                    rel, cands, cls):
            return {"steps": steps, "ok": False, "source_ids": src_ids}

    # pass 2: the 30d -> 7d correction against the claim id minted in pass 1
    if not _apply_correction(engine, wiki_root, src_ids, steps):
        return {"steps": steps, "ok": False, "source_ids": src_ids}

    step("build-pages", ["build-pages"])
    code, out = step("verify", ["verify"])
    ok = code == 0
    try:
        ok = ok and json.loads(out).get("ok", False)
    except ValueError:
        ok = False
    return {"steps": steps, "ok": ok, "source_ids": src_ids}
