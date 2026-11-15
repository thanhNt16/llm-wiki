#!/usr/bin/env python3
"""Unit tests for the rust-vs-python quality lane.

    python3 test_quality.py   (or python3 -m unittest test_quality)

Covers: corpus generator determinism (two generations byte-identical),
labels schema, scoring helpers (P/R/F1, semantic Jaccard), and the paired
permutation test on hand-crafted cases with known direction.

Stdlib only, Python >= 3.9.
"""
import filecmp
import json
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import gen_corpus  # noqa: E402
import run_quality  # noqa: E402
import significance  # noqa: E402

SMALL_DOCS, SMALL_FPD = 36, 4   # smallest size covering all planted cases


def _tree_bytes(root):
    out = {}
    for dirpath, _dirs, names in os.walk(root):
        for name in sorted(names):
            p = os.path.join(dirpath, name)
            rel = os.path.relpath(p, root)
            with open(p, "rb") as f:
                out[rel] = f.read()
    return out


class TestCorpusDeterminism(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="qc-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def test_two_gens_byte_identical(self):
        a = os.path.join(self.tmp, "a")
        b = os.path.join(self.tmp, "b")
        gen_corpus.generate(a, SMALL_DOCS, SMALL_FPD, gen_corpus.DEFAULT_SEED)
        gen_corpus.generate(b, SMALL_DOCS, SMALL_FPD, gen_corpus.DEFAULT_SEED)
        ta, tb = _tree_bytes(a), _tree_bytes(b)
        self.assertEqual(sorted(ta), sorted(tb))
        for rel in ta:
            self.assertEqual(ta[rel], tb[rel], "byte diff in %s" % rel)

    def test_different_seed_differs(self):
        a = os.path.join(self.tmp, "a")
        c = os.path.join(self.tmp, "c")
        gen_corpus.generate(a, SMALL_DOCS, SMALL_FPD, 1)
        gen_corpus.generate(c, SMALL_DOCS, SMALL_FPD, 2)
        self.assertNotEqual(_tree_bytes(a), _tree_bytes(c))


class TestLabelsSchema(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="qc-labels-")
        gen_corpus.generate(cls.tmp, SMALL_DOCS, SMALL_FPD,
                            gen_corpus.DEFAULT_SEED)
        with open(os.path.join(cls.tmp, "labels.json")) as f:
            cls.labels = json.load(f)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, True)

    def test_fact_schema(self):
        required = {"id", "doc_index", "subject", "predicate", "question",
                    "expected_value", "source_file", "locator_hint",
                    "correction_case", "conflict_case", "trap_case",
                    "in_summary"}
        for fact in self.labels["facts"]:
            self.assertTrue(required <= set(fact), fact["id"])
            self.assertIn("exact", fact["expected_value"])
            self.assertIn("regex", fact["expected_value"])
            self.assertTrue(
                os.path.isfile(os.path.join(
                    self.tmp, fact["source_file"])), fact["id"])

    def test_locator_resolves(self):
        """Every planted locator is a substring of its source doc."""
        for fact in self.labels["facts"]:
            with open(os.path.join(self.tmp, fact["source_file"])) as f:
                text = f.read()
            self.assertIn(fact["locator_hint"], text, fact["id"])

    def test_planted_cases(self):
        n_corr = sum(1 for f in self.labels["facts"]
                     if f["correction_case"])
        n_conf = sum(1 for f in self.labels["facts"]
                     if f["conflict_case"])
        self.assertGreaterEqual(n_corr, 3)
        self.assertGreaterEqual(n_conf, 4)   # 2 conflicts x 2 docs
        # every correction has a v2 doc on disk with a distinct value
        for fact in self.labels["facts"]:
            if not fact["correction_case"]:
                continue
            c = fact["correction"]
            v2p = os.path.join(self.tmp, c["file_v2"])
            self.assertTrue(os.path.isfile(v2p))
            self.assertNotEqual(fact["expected_value"]["exact"],
                                c["expected_value_v2"]["exact"])
            with open(v2p) as f:
                self.assertIn(c["locator_hint_v2"], f.read())

    def test_summary_exists(self):
        self.assertTrue(os.path.isfile(os.path.join(self.tmp,
                                                    "PROJECT.md")))


class TestScoring(unittest.TestCase):
    def test_prf(self):
        # tp=2 returned=3 expected=4 -> P=2/3 R=1/2 F1=0.5714
        p, r, f = run_quality.prf(2, 3, 4)
        self.assertAlmostEqual(p, 2 / 3)
        self.assertAlmostEqual(r, 0.5)
        self.assertAlmostEqual(f, 2 * (2 / 3) * 0.5 / (2 / 3 + 0.5))
        self.assertEqual(run_quality.prf(0, 0, 0), (1.0, 1.0, 1.0))
        self.assertEqual(run_quality.prf(0, 0, 5), (0.0, 0.0, 0.0))
        self.assertEqual(run_quality.prf(0, 3, 0), (0.0, 0.0, 0.0))

    def test_micro_prf(self):
        per_q = [{"tp": 1, "returned_n": 2, "expected_n": 1},
                 {"tp": 0, "returned_n": 1, "expected_n": 1}]
        p, r, f = run_quality.micro_prf(per_q)
        self.assertAlmostEqual(p, 1 / 3)
        self.assertAlmostEqual(r, 1 / 2)
        self.assertAlmostEqual(f, 0.4)

    def test_semantic_jaccard(self):
        self.assertEqual(
            run_quality.semantic_score("300", "300"), 1.0)
        self.assertEqual(
            run_quality.semantic_score("lru", "lfu"), 0.0)
        # "alice rivera" vs "alice rivera owns svc" -> 2 common of 4
        s = run_quality.semantic_score("Alice Rivera",
                                       "alice rivera owns svc")
        self.assertAlmostEqual(s, 0.5)

    def test_tokens_stopwords(self):
        self.assertNotIn("the", run_quality._tokens("the cat sat"))


class TestPermutation(unittest.TestCase):
    def test_known_direction(self):
        # a strictly dominates b on 8 discordant pairs -> p_better tiny
        # (only ~1/2^8 of permutations reach the observed diff) and
        # p_worse == 1.0 (obs is the max achievable diff).
        a = [1] * 8
        b = [0] * 8
        p_worse, p_better = significance.paired_permutation_p(
            a, b, seed=7, permutations=2000)
        self.assertEqual(p_worse, 1.0)
        self.assertLess(p_better, 0.02)

    def test_reversed_direction(self):
        a = [0] * 8
        b = [1] * 8
        p_worse, p_better = significance.paired_permutation_p(
            a, b, seed=7, permutations=2000)
        self.assertEqual(p_better, 1.0)
        self.assertLess(p_worse, 0.02)

    def test_equal_vectors(self):
        a = [1, 0, 1, 0]
        p_worse, p_better = significance.paired_permutation_p(
            a, list(a), seed=7, permutations=500)
        self.assertEqual(p_worse, 1.0)
        self.assertEqual(p_better, 1.0)

    def test_deterministic(self):
        a = [1, 0, 1, 1, 0, 1, 0, 1]
        b = [0, 0, 1, 0, 0, 1, 1, 0]
        r1 = significance.paired_permutation_p(a, b, seed=42,
                                               permutations=1000)
        r2 = significance.paired_permutation_p(a, b, seed=42,
                                               permutations=1000)
        self.assertEqual(r1, r2)


if __name__ == "__main__":
    unittest.main()
