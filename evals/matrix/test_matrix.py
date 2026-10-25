"""Matrix layer tests: generator determinism, planted-fault detection,
and the E6 double-run gate on a tiny engine pair (HEAD vs HEAD).

Runs the real engine CLI in temp dirs — this layer is the slow tier of
the eval stack (plan Task 13), not the unit suite.
"""
import json
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
for p in (REPO, os.path.join(REPO, "evals", "benchmarks"), HERE):
    if p not in sys.path:
        sys.path.insert(0, p)

try:
    from . import generator, runner
except ImportError:
    import generator  # noqa: E402
    import runner  # noqa: E402

HEAD_ENGINE = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")


def _fresh_dir():
    return tempfile.mkdtemp(prefix="llmwiki-matrix-test-")


class GeneratorDeterminismTest(unittest.TestCase):
    def test_same_seed_identical_tree(self):
        a = _fresh_dir()
        b = _fresh_dir()
        try:
            generator.synthetic_corpus(20, seed=7, dest=a)
            generator.synthetic_corpus(20, seed=7, dest=b)
            self.assertEqual(generator.tree_hash(a), generator.tree_hash(b))
        finally:
            shutil.rmtree(a, ignore_errors=True)
            shutil.rmtree(b, ignore_errors=True)

    def test_different_seed_differs(self):
        a = _fresh_dir()
        b = _fresh_dir()
        try:
            generator.synthetic_corpus(20, seed=7, dest=a)
            generator.synthetic_corpus(20, seed=8, dest=b)
            self.assertNotEqual(generator.tree_hash(a),
                                generator.tree_hash(b))
        finally:
            shutil.rmtree(a, ignore_errors=True)
            shutil.rmtree(b, ignore_errors=True)

    def test_corpus_shape_and_manifest(self):
        d = _fresh_dir()
        try:
            generator.synthetic_corpus(30, seed=1, dest=d)
            corpus = os.path.join(d, "corpus")
            files = []
            for sub in ("docs", "notes", "config"):
                files.extend(os.listdir(os.path.join(corpus, sub)))
            self.assertEqual(len(files), 30)
            with open(os.path.join(d, "manifest.json")) as f:
                m = json.load(f)
            self.assertTrue(m["gold_questions"])
            planted = [k for k, v in m["sources"].items() if v["claims"]]
            self.assertTrue(planted)
            # ~n/5 claim-bearing sources, spread over all three subdirs
            self.assertTrue(any(p.startswith("docs/") for p in planted))
            self.assertTrue(any(p.startswith("notes/") for p in planted))
            self.assertTrue(any(p.startswith("config/") for p in planted))
            # unanswerable probes present
            self.assertTrue(any(not q["expect"]
                                for q in m["gold_questions"]))
            # the deferred correction pair: forbid carries the legacy value
            corr = [q for q in m["gold_questions"] if q.get("forbid")]
            self.assertEqual(len(corr), 1)
        finally:
            shutil.rmtree(d, ignore_errors=True)


class FaultInjectionTest(unittest.TestCase):
    """Each fault mode must produce its planted doctor finding code on a
    built wiki (pages injected via filesystem, then `doctor`)."""

    def _doctor_codes(self, fault):
        wiki_root = _fresh_dir()
        self.addCleanup(shutil.rmtree, wiki_root, True)
        engine = runner.driver.Engine(HEAD_ENGINE)
        code, out, err = engine.run(["init", "--name", "faulttest"],
                                    wiki_root)
        self.assertEqual(code, 0, err)
        code, out, _ = engine.run(["build-pages"], wiki_root)
        self.assertEqual(code, 0)
        runner.apply_fault_pages(wiki_root,
                                 generator.fault_pages(fault, seed=3))
        code, out, err = engine.run(["doctor"], wiki_root)
        self.assertIsNotNone(out)
        data = json.loads(out)
        return {f["code"] for f in data["findings"]}

    def test_orphan_farm(self):
        self.assertIn("orphan_page", self._doctor_codes("orphan_farm"))

    def test_walled_garden(self):
        self.assertIn("walled_garden",
                      self._doctor_codes("walled_garden"))

    def test_stale_ttl_farm(self):
        self.assertIn("stale_page_ttl",
                      self._doctor_codes("stale_ttl_farm"))

    def test_near_dup_flood(self):
        self.assertIn("near_duplicate_subject",
                      self._doctor_codes("near_dup_flood"))


class ExtractTest(unittest.TestCase):
    def test_extract_head(self):
        path = runner.extract_engine("HEAD")
        try:
            self.assertTrue(os.path.isfile(path))
            with open(path) as f:
                self.assertIn("def main", f.read())
            # sibling wikicore package must ride along
            self.assertTrue(os.path.isdir(os.path.join(
                os.path.dirname(path), "wikicore")))
        finally:
            shutil.rmtree(path.split("/skill/")[0], ignore_errors=True)


class E6DeterminismTest(unittest.TestCase):
    def test_head_vs_head_identical_metrics(self):
        matrix = runner.run({"a": HEAD_ENGINE, "b": HEAD_ENGINE},
                            scenarios=["synthetic:6:9"],
                            experiments=["E1", "E6"])
        e6 = matrix["experiments"]["E6"]
        self.assertTrue(e6["deterministic"], e6.get("drift"))
        self.assertEqual(len(e6["cells_verified"]), 2)
        cells = matrix["experiments"]["E1"]["cells"]
        self.assertIn("a/synthetic-6-s9", cells)
        self.assertTrue(cells["a/synthetic-6-s9"]["build_ok"])


if __name__ == "__main__":
    unittest.main()
