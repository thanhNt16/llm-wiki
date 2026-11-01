"""SP3-T2 (#19): staging rejects unknown candidate keys.

Regression #462: a candidate written as {"subject","predicate","object"}
was silently normalized (``out = dict(c)`` kept ``object``, ``value``
defaulted to missing) and could become a claim with a null value. The
allowlist must name every key normalize_candidate reads/defaults — no more,
no less.
"""
import glob
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
WIKI = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")
sys.path.insert(0, os.path.join(REPO, "skill", "llm-wiki", "scripts"))

from wikicore import claims  # noqa: E402,F401 -- import first: breaks the
# transaction<->ftx<->claims cycle that shorthand alone would trip over.
from wikicore.shorthand import _ALLOWED_FIELDS, normalize_candidate  # noqa: E402
from wikicore.transaction import TxnError  # noqa: E402


def _run(root, *args):
    return subprocess.run([sys.executable, WIKI, "--root", root, *args],
                          capture_output=True, text=True)


class TestCandidateAllowlist(unittest.TestCase):
    def test_unknown_key_rejected(self):
        with self.assertRaises(TxnError) as ctx:
            normalize_candidate(
                {"subject": "a", "predicate": "b", "object": "c"},
                "s", 1, "s")
        msg = str(ctx.exception)
        self.assertIn("object", msg)
        self.assertIn("valid", msg)

    def test_all_documented_keys_accepted(self):
        samples = {
            "subject": "svc", "predicate": "role", "value": "api",
            "unit": "ms", "scope": {"env": "prod"},
            "locator": "s:svc runs api", "evidence": [],
            "authority": "doc:runbook",
            "status": "candidate", "recorded_at": "2026-10-31T00:00:00Z",
            "proposed_by": "agent", "root_origin": "root",
            "id": "auto", "valid_from": "2026-01-01",
            "valid_to": None, "supersedes": [],
        }
        self.assertEqual(set(samples), set(_ALLOWED_FIELDS),
                         "test fixture must exercise every allowed field")
        normalize_candidate(samples, "s", 1, "s")  # all keys at once
        for key in _ALLOWED_FIELDS:  # and each key on its own
            cand = {"subject": "svc", "predicate": "role", key: samples[key]}
            out = normalize_candidate(cand, "s", 1, "s")
            self.assertIsInstance(out, dict)

    def test_regression_462_null_claims_gone(self):
        root = tempfile.mkdtemp(prefix="allowlist-")
        self.addCleanup(lambda: __import__("shutil").rmtree(root, True))
        self.assertEqual(_run(root, "init", "--name", "t").returncode, 0)
        p = _run(root, "ingest", "--text", "x y z", "--text-ref", "n1")
        self.assertEqual(p.returncode, 0)
        source_id = json.loads(p.stdout)["source_id"]
        p = _run(root, "build")
        self.assertEqual(p.returncode, 3)  # blocked on extraction
        f = os.path.join(root, "bad-candidates.jsonl")
        with open(f, "w") as fh:
            fh.write('{"subject": "x", "predicate": "y", "object": "z"}\n')
        p = _run(root, "stage-candidates", "--file", f,
                 "--source-id", source_id, "--source-version", "1")
        self.assertEqual(p.returncode, 3, p.stdout + p.stderr)
        err = json.loads(p.stdout)["error"]
        self.assertIn("object", err)
        self.assertIn("valid", err)
        # nothing corrupt was staged/canonicalized
        self.assertEqual(glob.glob(os.path.join(root, ".llm-wiki",
                                                "claims", "*.json")), [])
        p = _run(root, "build")
        self.assertEqual(p.returncode, 3)  # still blocked, not corrupted
        self.assertEqual(json.loads(p.stdout)["step"], "extract")


if __name__ == "__main__":
    unittest.main()
