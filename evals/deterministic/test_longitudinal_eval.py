"""SP3-T7: longitudinal supported/stale/invented classifier eval — deterministic gate."""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
WIKI = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")
SCRIPT = os.path.join(REPO, "evals", "semantic", "longitudinal.py")
SCHEMA = os.path.join(REPO, "evals", "semantic", "results.schema.json")
SHA = "c0ffee1"


def _run(root, *args):
    return subprocess.run([sys.executable, WIKI, "--root", root, *args],
                          capture_output=True, text=True)


def _write_cands(pending, want_subject, lines):
    """Write shorthand candidates to the pending run whose source text matches."""
    for entry in pending:
        pend = os.path.join(os.path.dirname(entry["candidates_path"]),
                            "pending.json")
        with open(pend) as f:
            m = json.load(f)
        with open(m["content_path"]) as f:
            if want_subject in f.read():
                with open(entry["candidates_path"], "w") as f:
                    f.write(lines)
                return
    raise AssertionError("no pending run for %r" % want_subject)


def _built_wiki():
    """Two sources, three claims:
    - supported: accepted claim on a source whose pinned content is intact
    - stale: accepted claim whose pinned content file is later rewritten
    - invented: 'candidate' status + empty evidence list"""
    root = tempfile.mkdtemp(prefix="longeval-")
    assert _run(root, "init", "--name", "longeval").returncode == 0
    assert _run(root, "ingest", "--text", "postgres holds orders",
                "--text-ref", "n1").returncode == 0
    assert _run(root, "ingest", "--text", "mysql holds logs",
                "--text-ref", "n2").returncode == 0
    p = _run(root, "build")
    assert p.returncode == 3, p.stdout  # blocked: extraction pending
    pending = json.loads(p.stdout)["pending"]
    assert len(pending) == 2, pending
    _write_cands(pending, "postgres",
                 '{"subject": "postgres", "predicate": "role", '
                 '"value": "orders store", '
                 '"locator": "s:postgres holds orders"}\n'
                 # invented: non-auto-accept authority + explicit empty
                 # evidence -> stays status "candidate"
                 '{"subject": "rumor", "predicate": "says", '
                 '"value": "postgres eats disks", "evidence": [], '
                 '"authority": "inferred:guess"}\n')
    _write_cands(pending, "mysql",
                 '{"subject": "mysql", "predicate": "role", '
                 '"value": "logs store", '
                 '"locator": "s:mysql holds logs"}\n')
    p = _run(root, "build")
    assert p.returncode == 0, p.stdout[-800:]
    return root


def _drift_mysql_content(root):
    """Rewrite mysql's pinned normalized content so its locator/hash no
    longer resolves (same technique as test_evidence.test_verified_then_stale)."""
    src_dir = os.path.join(root, ".llm-wiki", "sources")
    for sid in os.listdir(src_dir):
        with open(os.path.join(src_dir, sid, "manifest.json")) as f:
            m = json.load(f)
        rel = m["versions"][-1]["normalized_path"]
        path = os.path.join(root, ".llm-wiki", *rel.split("/"))
        with open(path) as f:
            if "mysql" in f.read():
                with open(path, "w") as f:
                    f.write("totally different text now\n")
                return
    raise AssertionError("mysql source content not found")


class TestLongitudinalEval(unittest.TestCase):
    def setUp(self):
        self.root = _built_wiki()
        self.addCleanup(lambda: shutil.rmtree(self.root, True))
        _drift_mysql_content(self.root)
        self.out = os.path.join(self.root, "results.jsonl")

    def _schema_check(self, row):
        """Manual stdlib validation: required keys present + declared types."""
        with open(SCHEMA) as f:
            schema = json.load(f)
        for k in schema["required"]:
            self.assertIn(k, row, "missing required key %r" % k)
        types = {"string": str, "boolean": bool, "array": list}
        for k, spec in schema["properties"].items():
            if "type" in spec and k in row:
                self.assertIsInstance(row[k], types[spec["type"]],
                                      "%r wrong type" % k)
        self.assertTrue(re.match(r"^[0-9a-f]{7,40}$", row["engine_commit"]),
                        "engine_commit not hex 7-40")

    def test_supported_stale_invented_row(self):
        p = subprocess.run(
            [sys.executable, SCRIPT, "--root", self.root,
             "--engine-commit", SHA, "--out", self.out],
            capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr[-800:])
        with open(self.out) as f:
            rows = [json.loads(l) for l in f if l.strip()]
        self.assertEqual(len(rows), 1, "exactly one row appended")
        row = rows[0]
        self.assertEqual(row["scenario"], "longitudinal_classifier")
        self.assertEqual(row["engine_commit"], SHA)
        self.assertEqual(row["pass"], True)
        self.assertEqual(row["answer"], "supported=1 stale=1 invented=1")
        self.assertEqual(row["citations"], [])
        self.assertEqual(row["rubric_notes"],
                         "deterministic store-derived classification")
        self.assertIsInstance(row["timestamp"], str)
        self.assertTrue(row["timestamp"], "timestamp derived from claim data")
        self._schema_check(row)

    def test_appends_not_truncates(self):
        for _ in range(2):
            p = subprocess.run(
                [sys.executable, SCRIPT, "--root", self.root,
                 "--engine-commit", SHA, "--out", self.out],
                capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr[-800:])
        with open(self.out) as f:
            rows = [l for l in f if l.strip()]
        self.assertEqual(len(rows), 2)


if __name__ == "__main__":
    unittest.main()
