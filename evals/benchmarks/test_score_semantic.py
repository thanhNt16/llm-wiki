"""Tests for the two-arm semantic scorer.

Works both under `python3 -m unittest discover -s evals/benchmarks
-p "test_score*.py"` and package-style (`python3 -m unittest
evals.benchmarks.test_score_semantic`).
"""
import json
import os
import sys
import tempfile
import unittest

try:
    import score_semantic
except ImportError:  # package-style invocation: sibling dir not on sys.path
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import score_semantic


class ScorerTest(unittest.TestCase):
    def _run(self, rows, wiki_root=None):
        d = tempfile.mkdtemp()
        jl = os.path.join(d, "r.jsonl")
        with open(jl, "w") as f:
            for r in rows:
                f.write(json.dumps(r) + "\n")
        corpus = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                              "..", "golden-corpus")
        return score_semantic.main(jl, corpus, wiki_root)

    def test_citation_validity_and_values(self):
        res = self._run([
            {"arm": "grep", "scenario": "correction", "answerable": True,
             "answer": "The window is 7 days.", "citations": ["docs/adr-019-attribution.md:12"]},
            {"arm": "grep", "scenario": "correction", "answerable": True,
             "answer": "The window is 7 days.", "citations": ["docs/nope.md:1"]},
        ])
        self.assertTrue(res["rows"][0]["pass"])
        self.assertFalse(res["rows"][1]["pass"])
        self.assertEqual(res["summary"]["grep"]["by_check"]["citation_validity"], 1)

    def test_refusal_and_conflict(self):
        res = self._run([
            {"arm": "wiki", "scenario": "unanswerable", "answerable": False,
             "answer": "not documented", "citations": []},
            {"arm": "wiki", "scenario": "conflict", "answerable": True,
             "answer": "3 in one doc, 5 in another", "citations": []},
        ])
        self.assertTrue(res["rows"][0]["pass"])
        self.assertTrue(res["rows"][1]["checks"]["both_sides"])

    def test_answerable_row_with_empty_citations_fails(self):
        res = self._run([
            {"arm": "grep", "scenario": "correction", "answerable": True,
             "answer": "The window is 7 days.", "citations": []},
        ])
        self.assertFalse(res["rows"][0]["checks"]["citation_validity"])
        self.assertFalse(res["rows"][0]["pass"])

    def test_refusal_regex_not_tripped_by_hedged_answer(self):
        res = self._run([
            {"arm": "grep", "scenario": "unanswerable", "answerable": True,
             "answer": "We cannot find it; it's $10/mo", "citations": []},
            {"arm": "grep", "scenario": "unanswerable", "answerable": True,
             "answer": "This is not documented in the corpus.", "citations": []},
        ])
        self.assertFalse(res["rows"][0]["checks"]["refused"])
        self.assertTrue(res["rows"][1]["checks"]["refused"])

    def test_hyphenated_day_values(self):
        res = self._run([
            {"arm": "grep", "scenario": "correction", "answerable": True,
             "answer": "The window is a 7-day period.",
             "citations": ["docs/adr-019-attribution.md:12"]},
            {"arm": "grep", "scenario": "historical", "answerable": True,
             "answer": "Previously a 30-day window.",
             "citations": ["docs/adr-019-attribution.md:3"]},
        ])
        self.assertTrue(res["rows"][0]["checks"]["value_correct"])
        self.assertTrue(res["rows"][0]["pass"])
        self.assertTrue(res["rows"][1]["checks"]["value_correct"])


if __name__ == "__main__":
    unittest.main()
