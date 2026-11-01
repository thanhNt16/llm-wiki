#!/usr/bin/env python3
"""SP3-T7: longitudinal supported/stale/invented classifier eval.

Deterministic, stdlib-only, no network. Classifies every claim in a wiki
store from claim data alone:

  invented  iff status == "candidate" and the evidence list is empty
  stale     iff wikicore.evidence.check_evidence marks the claim
            stale/unresolved
  supported otherwise

Appends exactly one results.jsonl row whose timestamp is derived from
claim data (max updated_at/recorded_at) — never the wall clock. Exits 3
if the row violates the required keys/types of results.schema.json.

CLI:
  python3 evals/semantic/longitudinal.py --root WIKI --engine-commit SHA \
      [--out evals/semantic/results.jsonl]
"""
import argparse
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.abspath(
    os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))
sys.path.insert(0, SCRIPTS)

from wikicore import claims as claims_mod  # noqa: E402
from wikicore import evidence as evidence_mod  # noqa: E402
from wikicore.store import Wiki  # noqa: E402

SCHEMA_PATH = os.path.join(HERE, "results.schema.json")
COMMIT_RE = re.compile(r"^[0-9a-f]{7,40}$")

_JSON_TYPES = {"object": dict, "array": list, "string": str,
               "boolean": bool, "integer": int, "number": (int, float),
               "null": type(None)}


def _validate_row(row):
    """Manual stdlib validation of results.schema.json: required keys
    present + declared property types (no jsonschema dependency)."""
    try:
        with open(SCHEMA_PATH) as f:
            schema = json.load(f)
    except (OSError, ValueError) as e:
        return ["cannot read schema %s: %s" % (SCHEMA_PATH, e)]
    if not isinstance(row, dict):
        return ["row is not a JSON object"]
    errors = []
    for k in schema.get("required", []):
        if k not in row:
            errors.append("missing required key %r" % k)
    for k, spec in schema.get("properties", {}).items():
        t = spec.get("type")
        if t and k in row and not isinstance(row[k], _JSON_TYPES[t]):
            errors.append("key %r is not %s" % (k, t))
    if "engine_commit" in row and not (
            isinstance(row["engine_commit"], str)
            and COMMIT_RE.match(row["engine_commit"])):
        errors.append("engine_commit not hex 7-40: %r"
                      % (row["engine_commit"],))
    return errors


def classify(wiki):
    """({supported, stale, invented}, timestamp) over all store claims."""
    all_claims = claims_mod.list_claims(wiki)
    ev = evidence_mod.check_evidence(wiki, details_limit=10 ** 9)
    not_verified = {d["claim_id"] for d in ev.get("details", [])}
    counts = {"supported": 0, "stale": 0, "invented": 0}
    for c in all_claims:
        if c.get("status") == "candidate" and not c.get("evidence"):
            counts["invented"] += 1
        elif c.get("id") in not_verified:
            counts["stale"] += 1
        else:
            counts["supported"] += 1
    stamps = [v for c in all_claims
              for v in (c.get("updated_at"), c.get("recorded_at")) if v]
    return counts, (max(stamps) if stamps else "")


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="longitudinal supported/stale/invented classifier eval")
    ap.add_argument("--root", required=True, help="wiki root")
    ap.add_argument("--engine-commit", required=True,
                    help="git SHA of the engine under test")
    ap.add_argument("--out", default=os.path.join(HERE, "results.jsonl"),
                    help="results.jsonl to append (default: alongside script)")
    args = ap.parse_args(argv)

    wiki = Wiki(args.root)
    if not wiki.exists():
        print("error: not a wiki root: %s" % args.root, file=sys.stderr)
        return 2

    counts, stamp = classify(wiki)
    row = {
        "scenario": "longitudinal_classifier",
        "timestamp": stamp,
        "engine_commit": args.engine_commit,
        "pass": True,
        "answer": "supported=%(supported)d stale=%(stale)d "
                  "invented=%(invented)d" % counts,
        "citations": [],
        "rubric_notes": "deterministic store-derived classification",
    }
    errors = _validate_row(row)
    if errors:
        for e in errors:
            print("schema violation: %s" % e, file=sys.stderr)
        return 3

    out = os.path.abspath(args.out)
    parent = os.path.dirname(out)
    if parent:
        os.makedirs(parent, exist_ok=True)
    with open(out, "a", encoding="utf-8") as f:
        f.write(json.dumps(row, ensure_ascii=False) + "\n")
    print(json.dumps(row, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
