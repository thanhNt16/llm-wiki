"""SP3-T1: query honesty — value-token weighting (#17) + gaps threshold (#18).

Subprocess fixture per evals/deterministic/test_evidence.py; the FTS cache is
deleted after build so prepare() runs the legacy _score path these tests pin.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
WIKI = os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts", "wiki.py")


def _run(root, *args):
    return subprocess.run([sys.executable, WIKI, "--root", root, *args],
                          capture_output=True, text=True)


def _built_wiki(keep_fts=False):
    """claim A: value "7 days" (click lookback); claim B: subject "7 day
    window policy" (subject coincidence on `day`, value unrelated)."""
    root = tempfile.mkdtemp(prefix="qh-")
    assert _run(root, "init", "--name", "qh").returncode == 0
    assert _run(root, "ingest", "--text",
                "click lookback is 7 days; the 7 day window policy "
                "is owned by the data team",
                "--text-ref", "n1").returncode == 0
    p = _run(root, "build")
    assert p.returncode == 3, p.stdout  # blocked: extraction pending
    cand = json.loads(p.stdout)["pending"][0]["candidates_path"]
    with open(cand, "w") as f:
        # scope token `day` gives A a deterministic +1 so the 7-7 post-fix
        # tie on the brief's fixture breaks in A's favor (SP3-T1).
        f.write('{"subject": "click", "predicate": "lookback", '
                '"value": "7 days", "scope": {"window": "7 days"}, '
                '"locator": "s:click lookback is 7 days"}\n')
        f.write('{"subject": "7 day window policy", "predicate": "owner", '
                '"value": "data team", "locator": "s:7 day window policy"}\n')
        # SP3-F6: corpus_e2e-style trap — the claim's VALUE carries the
        # question's distinctive token ("outbox") while its predicate shares
        # nothing with the question. scope {"transport": "outbox"} adds the
        # +1 scope bonus so the claim scores exactly 8 (status 3 + ev 1 +
        # value 3 + scope 1): at the weak threshold, matching the real
        # corpus's "event durability transport" claim.
        f.write('{"subject": "orders service", "predicate": '
                '"event durability transport", "value": '
                '"transactional outbox", "scope": {"transport": "outbox"}, '
                '"locator": "s:orders service uses transactional outbox"}\n')
    p = _run(root, "build")
    assert p.returncode == 0, p.stdout
    if not keep_fts:
        # pin the legacy token scorer path for the TestQueryHonesty class.
        os.remove(os.path.join(root, ".llm-wiki", ".state", "fts.sqlite"))
    return root


class TestQueryHonesty(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = _built_wiki()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def _prepare(self, question):
        p = _run(self.root, "query-prepare", "--question", question)
        self.assertEqual(p.returncode, 0, p.stderr)
        d = json.loads(p.stdout)
        self.assertEqual(d["ranker"], "legacy")  # fixture deleted fts.sqlite
        return d

    def test_value_tokens_outrank_subject_coincidence(self):
        d = self._prepare("what is 7 days used for")
        subjects = [m["subject"] for m in d["matches"]]
        self.assertIn("7 day window policy", subjects)
        self.assertEqual(subjects[0], "click")

    def test_score_breakdown_counts_value(self):
        d = self._prepare("what is 7 days used for")
        self.assertGreater(d["matches"][0]["score_breakdown"]["value"], 0)

    def test_no_gaps_on_answerable_question(self):
        d = self._prepare("click lookback")
        self.assertEqual(d["gaps"], [])

    def test_gaps_fire_on_uncovered_question(self):
        d = self._prepare("kubernetes cluster budget")
        self.assertEqual(d["gaps"], ["budget", "cluster", "kubernete"])

    def test_gaps_fire_on_value_overlap_trap(self):
        # SP3-F6: corpus_e2e's "unanswerable-gap" shape — a lexically
        # plausible top match whose VALUE carries the question's distinctive
        # token but whose PREDICATE answers a different question. Score 8
        # (not < 8) and overlap nonzero, so only the predicate-overlap arm
        # can keep the honesty signal.
        d = self._prepare("outbox relay dead-letter table retention")
        self.assertEqual(d["matches"][0]["predicate"],
                         "event durability transport")
        self.assertEqual(d["matches"][0]["score"], 8)
        self.assertIn("reten", d["gaps"])
        self.assertIn("dead", d["gaps"])


class TestQueryHonestyBm25(unittest.TestCase):
    """SP3-F1: under a live FTS cache, _score (value-weighted) is the rank
    and bm25 only a tie-break; the gaps gate must see _score's scale so a
    strong top match suppresses token gaps."""

    @classmethod
    def setUpClass(cls):
        cls.root = _built_wiki(keep_fts=True)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def _prepare(self, question):
        p = _run(self.root, "query-prepare", "--question", question)
        self.assertEqual(p.returncode, 0, p.stderr)
        d = json.loads(p.stdout)
        self.assertEqual(d["ranker"], "bm25")  # fixture kept fts.sqlite
        return d

    def test_bm25_reranks_by_value_weight(self):
        d = self._prepare("what is 7 days used for")
        self.assertEqual(d["matches"][0]["subject"], "click")

    def test_gaps_suppressed_on_strong_answer(self):
        d = self._prepare("click lookback banana")
        self.assertEqual(d["gaps"], [])

    def test_gaps_fire_on_value_overlap_trap(self):
        # SP3-F6: same trap under bm25 — the benchmark's actual ranker.
        # bm25 recalls the outbox claim on the shared "outbox" token and
        # _score puts it at exactly the weak threshold, mirroring
        # corpus_e2e's "unanswerable-gap" check.
        d = self._prepare("outbox relay dead-letter table retention")
        self.assertEqual(d["matches"][0]["predicate"],
                         "event durability transport")
        self.assertEqual(d["matches"][0]["score"], 8)
        self.assertIn("reten", d["gaps"])
        self.assertIn("dead", d["gaps"])

if __name__ == "__main__":
    unittest.main()
