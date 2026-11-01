"""SP2a Task 3: doctor reports FTS health + --rebuild-fts — TDD test."""
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import doctor, ftx  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402


class TestDoctorFts(unittest.TestCase):
    def test_health_reported_and_rebuildable(self):
        w = Wiki(tempfile.mkdtemp(prefix="ftx-doc-"))
        try:
            init_wiki(w, "ftx-doc")
            r = doctor.run(w)
            self.assertIn("fts", r)
            self.assertEqual(r["fts"]["reason"], "missing")
            self.assertEqual(r["fts"]["usable"], False)
            out = ftx.rebuild(w)
            self.assertEqual(out["rows"], 0)
            h = doctor.run(w)["fts"]
            self.assertEqual(h["reason"], "ok")
            self.assertEqual(h["usable"], True)
        finally:
            shutil.rmtree(w.root, ignore_errors=True)

    def test_stale_reported_as_not_ok_but_not_fatal(self):
        w = Wiki(tempfile.mkdtemp(prefix="ftx-doc2-"))
        try:
            init_wiki(w, "ftx-doc2")
            ftx.rebuild(w)
            st = w.state()
            st["revision"] += 1
            w.save_state(st)
            r = doctor.run(w)
            self.assertEqual(r["fts"]["reason"], "stale_revision")
            # a stale cache degrades retrieval, it must not brick the wiki:
            # doctor flags it (exit 3 happens only for errors), engine falls
            # back to the legacy ranker meanwhile
            self.assertIn("fts_stale", [f["code"] for f in r["findings"]])
        finally:
            shutil.rmtree(w.root, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
