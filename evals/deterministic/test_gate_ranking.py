"""SP2a Gate R: frozen ranking expectations for the query battery.

Regenerates the deterministic corpus in a temp dir, runs the battery, and
fails on ANY drift from expected.json (top-1 subject and top-5 ordering per
question). This pins retrieval behavior: scorer changes must update
expected.json deliberately, never silently.
"""
import json
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
BATT = os.path.join(HERE, "..", "query-battery")
sys.path.insert(0, os.path.join(BATT, "..", "..", "skill", "llm-wiki", "scripts"))
sys.path.insert(0, BATT)

import gen_corpus  # noqa: E402
import run_battery  # noqa: E402


class TestGateRanking(unittest.TestCase):
    def test_rankings_match_frozen_expectations(self):
        root = tempfile.mkdtemp(prefix="gate-r-")
        try:
            gen_corpus.build(root)
            got = run_battery.main(root)
            exp = json.load(open(os.path.join(BATT, "expected.json")))
            self.assertEqual(got["ranker"], exp["ranker"],
                             "ranker changed: cache fallback drift?")
            for q in got["per_q"]:
                e = exp["questions"][q["qid"]]
                self.assertEqual(q["returned_subjects"][0], e["top1"],
                                 "%s top-1 drift" % q["qid"])
                self.assertEqual(q["returned_subjects"], e["top5"],
                                 "%s top-5 drift" % q["qid"])
        finally:
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
