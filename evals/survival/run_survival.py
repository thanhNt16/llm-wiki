#!/usr/bin/env python3
"""Survival + trap lane: planted-fact recall by position, correction
propagation, corroboration/scope twins, shared-value and near-subject traps
(crow-hq long2/dup patterns adapted to the claim model), plus N-run
stability and cross-engine structural parity.

    python3 evals/survival/run_survival.py [-e python|rust] [--runs 3]
                                          [--json survival.json] [--keep]

Builds the corpus through the real CLI (init/ingest/stage-candidates/
reconcile-prepare/reconcile-apply), validates the final claim state against
corpus/labels.json, and merges results into the JSON report keyed by engine.
When both engines are present in the report, structural parity is asserted
(canonical claim graph: ids/ULIDs normalized away). Exit 0 iff every check
passes for the requested engine (and parity, once both are present).
"""
import argparse
import glob
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

_HERE = os.path.dirname(os.path.abspath(__file__))
_EVALS = os.path.dirname(_HERE)
sys.path.insert(0, _EVALS)
import _engine  # noqa: E402

CORPUS = os.path.join(_HERE, "corpus")
DEFAULT_JSON = os.path.join(_HERE, "survival.json")
_ULID = re.compile(r"claim_[0-9A-HJKMNP-TV-Z]{26}|source_[0-9A-HJKMNP-TV-Z]{26}"
                   r"|run_[0-9A-HJKMNP-TV-Z]{26}|item_[0-9A-HJKMNP-TV-Z]{26}")


def _load(path):
    with open(path) as f:
        return json.load(f)


def _key(subject, predicate):
    return "%s/%s" % (str(subject).strip().lower(),
                      str(predicate).strip().lower())


def _scope_key(scope):
    return json.dumps({k: str(v) for k, v in (scope or {}).items()},
                      sort_keys=True)


def run_cli(argv, args, cwd, input_text=None):
    p = subprocess.run(argv + list(args), cwd=cwd, capture_output=True,
                       text=True, input=input_text)
    return p.returncode, p.stdout, p.stderr


def build(engine, project, manifest):
    """ingest + stage + prepare + apply every batch through the real CLI."""
    steps = []
    batches = {}
    for cand_file in sorted(glob.glob(os.path.join(CORPUS, "candidates",
                                                   "*.jsonl"))):
        name = os.path.basename(cand_file)[3:-6]  # strip NN- and .jsonl
        batches[name] = [json.loads(l) for l in open(cand_file) if l.strip()]

    code, out, err = run_cli(engine, ["init", "--name", "survival"], project)
    if code != 0:
        raise RuntimeError("init exit=%d: %s" % (code, err[:200]))

    def ingest(rel):
        code, out, err = run_cli(engine, ["ingest", "--file",
                                          os.path.join(CORPUS, rel)],
                                 project)
        if code != 0:
            raise RuntimeError("ingest %s exit=%d: %s" % (rel, code, err[:200]))
        return json.loads(out)["source_id"]

    def claim_by(triple):
        """claim id for (key, scope, value) among current claims."""
        for path in glob.glob(os.path.join(project, ".llm-wiki", "claims",
                                           "claim_*.json")):
            c = _load(path)
            if (_key(c["subject"], c["predicate"]) == triple[0]
                    and _scope_key(c.get("scope")) == triple[1]
                    and c.get("value") == triple[2]):
                return c["id"]
        return None

    for i, rel in enumerate(manifest["ingest_order"]):
        name = os.path.basename(rel)[:-3]
        sid = ingest(rel)
        cands = batches[name]
        for c in cands:
            c["evidence"][0]["source_id"] = sid
        with tempfile.NamedTemporaryFile("w", suffix=".json",
                                         delete=False) as f:
            json.dump(cands, f)
            cand_path = f.name
        report = {"source_id": sid, "source_version": 1,
                  "coverage": {"text": "complete"}, "warnings": [],
                  "parser": {"name": "survival", "version": "1"}}
        with tempfile.NamedTemporaryFile("w", suffix=".json",
                                         delete=False) as f:
            json.dump(report, f)
            report_path = f.name
        code, out, err = run_cli(
            engine, ["stage-candidates", "--file", cand_path,
                     "--source-id", sid, "--source-version", "1",
                     "--report", report_path], project)
        if code != 0:
            raise RuntimeError("stage %s exit=%d: %s" % (rel, code,
                                                         (out + err)[:300]))
        run_id = json.loads(out)["run_id"]
        code, out, err = run_cli(engine, ["reconcile-prepare",
                                          "--run", run_id], project)
        if code != 0:
            raise RuntimeError("prepare %s exit=%d: %s" % (rel, code,
                                                           (out + err)[:300]))
        # batch classifications live in labels.json order-independent:
        # explicit ones are attached in make_corpus via cls markers
        cls = json.loads(json.dumps(CLASSIFICATIONS.get(name, [])))
        for c in cls:
            tgt = c.get("target_claim_id", "")
            if isinstance(tgt, str) and tgt.startswith("RESOLVE:"):
                subj, pred, val = tgt[len("RESOLVE:"):].split("/")
                cid = claim_by((_key(subj, pred), _scope_key({}), _json_val(val)))
                if cid is None:
                    raise RuntimeError("cannot resolve target %s" % tgt)
                c["target_claim_id"] = cid
        with tempfile.NamedTemporaryFile("w", suffix=".json",
                                         delete=False) as f:
            json.dump(cls, f)
            cls_path = f.name
        code, out, err = run_cli(
            engine, ["reconcile-apply", "--run", run_id,
                     "--classifications", cls_path], project)
        steps.append({"doc": rel, "stage": code == 0,
                      "err": "" if code == 0 else (out + err)[-300:]})
        if code != 0:
            raise RuntimeError("apply %s exit=%d: %s" % (rel, code,
                                                         (out + err)[:300]))
    return steps


def _json_val(raw):
    try:
        return json.loads(raw)
    except ValueError:
        return raw


# explicit classifications keyed by doc basename (mirrors make_corpus)
CLASSIFICATIONS = {
    "platform-report-addendum": [
        {"index": 0, "relationship": "CORRECTION",
         "target_claim_id": "RESOLVE:attribution/window-days/30",
         "valid_from": "2026-09-01", "valid_to": None,
         "authority": {"type": "authoritative_source"}}],
    "retention-change": [
        {"index": 0, "relationship": "CORRECTION",
         "target_claim_id": "RESOLVE:event-retention/retention-days/90",
         "valid_from": "2026-10-01", "valid_to": None,
         "authority": {"type": "authoritative_source"}}],
    "scope-sandbox": [
        {"index": 0, "relationship": "SCOPE_DIFFERENCE",
         "target_claim_id": None, "valid_from": None, "valid_to": None,
         "authority": {"type": "authoritative_source"}}],
}


def load_state(project):
    claims = []
    for path in glob.glob(os.path.join(project, ".llm-wiki", "claims",
                                       "claim_*.json")):
        claims.append(_load(path))
    queue = []
    qpath = os.path.join(project, ".llm-wiki", ".state", "review-queue.jsonl")
    if os.path.isfile(qpath):
        queue = [json.loads(l) for l in open(qpath) if l.strip()]
    return claims, queue


def canonical(claims):
    """Structure hash: ids/timestamps normalized to label keys."""
    by_id = {c["id"]: c for c in claims}
    out = []
    for c in sorted(claims, key=lambda c: (_key(c["subject"], c["predicate"]),
                                           _scope_key(c.get("scope")),
                                           json.dumps(c.get("value")))):
        sup = c.get("superseded_by")
        sup_canon = None
        if sup in by_id:
            t = by_id[sup]
            sup_canon = "%s=%s" % (_key(t["subject"], t["predicate"]),
                                   json.dumps(t.get("value")))
        out.append({
            "key": _key(c["subject"], c["predicate"]),
            "scope": json.loads(_scope_key(c.get("scope"))),
            "value": c.get("value"), "status": c.get("status"),
            "superseded_by": sup_canon,
            "authority": c.get("authority", {}).get("type"),
            "valid_from": c.get("valid_from"),
            "evidence": [{"source_version": e.get("source_version"),
                          "locator": e.get("locator")}
                         for e in c.get("evidence", [])],
        })
    blob = json.dumps(out, sort_keys=True, ensure_ascii=False)
    # review items: strip ids/timestamps, keep kind + id-free problem text
    return hashlib.sha256(blob.encode()).hexdigest(), out


def measure(project, manifest):
    """All label checks. Returns (checks list, claims) — no exceptions."""
    claims, queue = load_state(project)
    labels = manifest["labels"]
    by_key = {}
    for c in claims:
        by_key.setdefault(_key(c["subject"], c["predicate"]), []).append(c)
    checks = []

    def find(label, by_value=True):
        key = _key(label["subject"], label["predicate"])
        want = [c for c in by_key.get(key, [])
                if _scope_key(c.get("scope")) == _scope_key(label["scope"])]
        if by_value and "value" in label:
            want = [c for c in want if c.get("value") == label["value"]]
        return want

    for lab in labels:
        cid = lab["id"]
        if lab.get("status") == "rejected_tier_match":
            # nothing canonical written from the near-spelling: every
            # claim under the key keeps the canonical subject spelling,
            # and the tier match is parked as a review item
            key = _key(lab["subject"], lab["predicate"])
            stray = [c for c in by_key.get(key, [])
                     if str(c["subject"]).strip().lower() != key.split("/")[0]]
            reviewed = any("normalization match" in str(i.get("problem", ""))
                           for i in queue)
            ok = not stray and reviewed
            checks.append({"id": cid, "pass": ok,
                           "detail": "no claim written; review item present"
                           if ok else "stray=%s reviewed=%s"
                           % ([c["subject"] for c in stray], reviewed)})
            continue
        want = find(lab)
        if not want:
            checks.append({"id": cid, "pass": False,
                           "detail": "no claim for key"})
            continue
        c = want[0]
        problems = []
        if c.get("status") != lab["status"]:
            problems.append("status=%s want=%s" % (c.get("status"),
                                                   lab["status"]))
        if c.get("value") != lab["value"]:
            problems.append("value=%r want=%r" % (c.get("value"),
                                                  lab["value"]))
        if len(c.get("evidence", [])) != lab["evidence_count"]:
            problems.append("evidence=%d want=%d"
                            % (len(c.get("evidence", [])),
                               lab["evidence_count"]))
        if lab.get("superseded_by"):
            canon = lab["superseded_by"]
            targets = [l for l in labels if l["id"] == canon]
            sup_ok = False
            if targets:
                sup_ok = any(
                    x.get("status") == "superseded"
                    and x.get("superseded_by")
                    for x in find({"subject": lab["subject"],
                                   "predicate": lab["predicate"],
                                   "scope": lab["scope"],
                                   "value": lab["value"]}))
            if not sup_ok:
                problems.append("superseded_by missing")
        if lab.get("locator_line"):
            loc = c["evidence"][0].get("locator", {})
            if loc.get("line_start") != lab["locator_line"]:
                problems.append("locator=%r want line %s"
                                % (loc.get("line_start"),
                                   lab["locator_line"]))
        if lab.get("evidence_count") == 2:
            srcs = {e.get("source_id") for e in c.get("evidence", [])}
            if len(srcs) != 2:
                problems.append("corroboration sources not distinct")
        checks.append({"id": cid, "pass": not problems,
                       "detail": "; ".join(problems) or "ok"})

    # scope twins: both accepted, no cross-supersede
    twins = [l for l in labels if l["kind"] == "scope-twin"]
    if len(twins) == 2:
        both = by_key.get(_key(twins[0]["subject"],
                               twins[0]["predicate"]), [])
        ok = (len(both) == 2
              and all(c.get("status") == "accepted" for c in both)
              and {json.dumps(c.get("scope"), sort_keys=True)
                   for c in both} == {json.dumps(t["scope"], sort_keys=True)
                                      for t in twins})
        checks.append({"id": "scope-twins-both-live", "pass": ok,
                       "detail": "2 scoped claims, both accepted" if ok
                       else [(c.get("scope"), c.get("status")) for c in both]})

    buckets = {}
    for lab in labels:
        if lab["kind"] == "planted":
            b = lab.get("bucket") or "?"
            chk = next(c for c in checks if c["id"] == lab["id"])
            buckets.setdefault(b, []).append(chk["pass"])
    bucket_report = {b: {"planted": len(v), "survived": sum(v)}
                     for b, v in sorted(buckets.items())}
    chash, cstruct = canonical(claims)
    return checks, bucket_report, chash, cstruct


def main():
    ap = argparse.ArgumentParser()
    _engine.add_engine_arg(ap)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--json", default=DEFAULT_JSON)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()

    engine = _engine.engine_argv(a.engine)
    ename = _engine.engine_name(a.engine)
    manifest = _load(os.path.join(CORPUS, "labels.json"))

    projects = []
    try:
        for i in range(max(1, a.runs)):
            proj = tempfile.mkdtemp(prefix="survival-%s-%d-" % (ename, i))
            projects.append(proj)
            build(engine, proj, manifest)
    finally:
        pass

    checks, buckets, chash, cstruct = measure(projects[0], manifest)
    stability = {"runs": len(projects), "stable": True, "hashes": []}
    for proj in projects[1:]:
        _, _, h2, _ = measure(proj, manifest)
        stability["hashes"].append(h2)
        if h2 != chash:
            stability["stable"] = False
    stability["hashes"].insert(0, chash)

    failed = [c for c in checks if not c["pass"]]
    result = {
        "engine": ename, "generated_at": _now(),
        "checks": checks, "buckets": buckets,
        "stability": stability, "canonical_hash": chash,
        "verdict": "pass" if not failed and stability["stable"] else "fail",
        "failed": [c["id"] for c in failed],
    }

    report = {}
    if os.path.isfile(a.json):
        try:
            report = _load(a.json)
        except ValueError:
            report = {}
    report[ename] = result
    engines = sorted(k for k in report if not k.startswith("_"))
    parity = None
    if len(engines) == 2:
        same = (report[engines[0]]["canonical_hash"]
                == report[engines[1]]["canonical_hash"])
        parity = {"engines": engines, "structure_equal": same}
        if not same:
            report[engines[0]]["verdict"] = "fail"
            report[engines[1]]["verdict"] = "fail"
    report["_parity"] = parity
    with open(a.json, "w") as f:
        json.dump(report, f, indent=2, ensure_ascii=False)

    print(json.dumps({"engine": ename, "verdict": result["verdict"],
                      "failed": result["failed"], "buckets": buckets,
                      "stability": stability["stable"],
                      "canonical_hash": chash, "parity": parity},
                     indent=2))
    if not a.keep:
        for p in projects:
            shutil.rmtree(p, ignore_errors=True)
    sys.exit(0 if result["verdict"] == "pass" else 1)


def _now():
    import datetime
    return datetime.datetime.now(datetime.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ")


if __name__ == "__main__":
    main()
