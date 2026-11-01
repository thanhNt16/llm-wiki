"""SP2d: `wiki.py build` one-command pipeline — deterministic gates."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
WIKI = os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts", "wiki.py")


def _run(root, *args):
    return subprocess.run([sys.executable, WIKI, "--root", root, *args],
                          capture_output=True, text=True)


class TestBuildPipeline(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="build-gate-")
        self.addCleanup(lambda: __import__("shutil").rmtree(self.root, True))
        r = _run(self.root, "init", "--name", "gate")
        self.assertEqual(r.returncode, 0, r.stderr)
        r = _run(self.root, "ingest", "--text", "postgres holds orders",
                 "--text-ref", "n1")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_blocks_on_unstaged_extraction(self):
        p = _run(self.root, "build")
        self.assertEqual(p.returncode, 3)
        d = json.loads(p.stdout)
        self.assertEqual(d["phase"], "blocked")
        self.assertEqual(d["step"], "extract")
        self.assertEqual(len(d["pending"]), 1)
        self.assertFalse(os.path.isfile(d["pending"][0]["candidates_path"]))
        self.assertIn("candidates", d["next"])

    def test_done_after_staging_and_idempotent(self):
        p = _run(self.root, "build")
        cand = json.loads(p.stdout)["pending"][0]["candidates_path"]
        with open(cand, "w") as f:
            f.write('{"subject": "postgres", "predicate": "role", '
                    '"value": "orders store", '
                    '"locator": "s:postgres holds orders"}\n')
                    # SP3-T3: "quote" dropped — SP3-T2 allowlist rejects it
        p = _run(self.root, "build")
        self.assertEqual(p.returncode, 0, p.stdout[-400:])
        d = json.loads(p.stdout)
        self.assertEqual(d["phase"], "done")
        self.assertTrue(d["verify"]["ok"])
        self.assertEqual(d["compile"]["changes"]["claims_created"], 1)
        # idempotent: replay path, still done + verify ok + exit 0
        p2 = _run(self.root, "build")
        self.assertEqual(p2.returncode, 0, p2.stdout[-400:])
        self.assertEqual(json.loads(p2.stdout)["phase"], "done")


if __name__ == "__main__":
    unittest.main()
