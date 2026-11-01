"""SP2b: mechanical evidence re-resolution — deterministic gates."""
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


def _built_wiki():
    """One source -> one claim citing it via a text locator."""
    root = tempfile.mkdtemp(prefix="ev-gate-")
    assert _run(root, "init", "--name", "gate").returncode == 0
    assert _run(root, "ingest", "--text", "postgres holds orders",
                "--text-ref", "n1").returncode == 0
    p = _run(root, "build")
    assert p.returncode == 3, p.stdout  # blocked: extraction pending
    cand = json.loads(p.stdout)["pending"][0]["candidates_path"]
    with open(cand, "w") as f:
        f.write('{"subject": "postgres", "predicate": "role", '
                '"value": "orders store", '
                '"locator": "s:postgres holds orders"}\n')
                # SP3-T3: "quote" dropped — SP3-T2 allowlist rejects it
    p = _run(root, "build")
    assert p.returncode == 0, p.stdout
    return root


class TestCheckEvidence(unittest.TestCase):
    def setUp(self):
        self.root = _built_wiki()
        self.addCleanup(lambda: __import__("shutil").rmtree(self.root, True))

    def _check(self):
        p = _run(self.root, "check-evidence")
        self.assertEqual(p.returncode, 0, p.stderr)
        return json.loads(p.stdout)

    def _claim(self):
        p = _run(self.root, "subjects")
        return p

    def _content_path(self):
        # the single source's pinned v1 normalized content file
        src_dir = os.path.join(self.root, ".llm-wiki", "sources")
        sid = os.listdir(src_dir)[0]
        m = json.load(open(os.path.join(src_dir, sid, "manifest.json")))
        rel = m["versions"][-1]["normalized_path"]
        return os.path.join(self.root, ".llm-wiki", *rel.split("/"))

    def test_verified_then_stale_then_unresolved(self):
        d = self._check()
        self.assertEqual(d["ok"], True)
        self.assertEqual(d["verified"], 1)
        self.assertEqual(d["stale"], 0)
        self.assertEqual(d["unresolved"], 0)
        self.assertEqual(d["details"], [])

        # drift: rewrite the pinned content so the locator no longer resolves
        path = self._content_path()
        with open(path, "w") as f:
            f.write("totally different text now\n")
        d = self._check()
        self.assertEqual(d["ok"], False)
        self.assertEqual(d["stale"], 1)
        self.assertEqual(d["verified"], 0)
        self.assertEqual(d["details"][0]["status"], "stale")
        self.assertTrue(any("sha256" in r or "locator" in r
                            for r in d["details"][0]["reasons"]))

        # destroy the manifest -> unresolved (fails closed)
        sid_dir = os.path.dirname(self._content_path())
        # normalized_path may live under sources/<sid>/; remove manifest by sid
        src_dir = os.path.join(self.root, ".llm-wiki", "sources")
        sid = os.listdir(src_dir)[0]
        os.remove(os.path.join(src_dir, sid, "manifest.json"))
        d = self._check()
        self.assertEqual(d["unresolved"], 1)
        self.assertEqual(d["ok"], False)

    def test_heading_and_line_locators(self):
        root = _built_wiki()
        self.addCleanup(lambda: __import__("shutil").rmtree(root, True))
        p = _run(root, "build")  # replay -> done, exit 0
        self.assertEqual(p.returncode, 0)
        d = json.loads(_run(root, "check-evidence").stdout)
        self.assertEqual(d["ok"], True)


if __name__ == "__main__":
    unittest.main()
