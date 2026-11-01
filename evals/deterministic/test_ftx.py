"""SP2a Task 1: FTS5 cache core (wikicore.ftx) — TDD tests."""
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import claims, ftx, ids  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import Transaction  # noqa: E402


def _mk_wiki(claim_rows=()):
    w = Wiki(tempfile.mkdtemp(prefix="ftx-t-"))
    init_wiki(w, "ftx-test")
    txn = Transaction(w, "test-seed")
    for subj, pred, val in claim_rows:
        claims.save_claim(txn, {"id": ids.new("claim"), "subject": subj,
                                "predicate": pred, "value": val,
                                "status": "provisional", "evidence": [],
                                "authority": {"type": "authoritative_source"}})
    txn.commit(w.revision())
    return w


class TestFtx(unittest.TestCase):
    def setUp(self):
        self._ws = []

    def tearDown(self):
        for w in self._ws:
            shutil.rmtree(w.root, ignore_errors=True)

    def _wiki(self, rows):
        w = _mk_wiki(rows)
        self._ws.append(w)
        return w

    def test_rebuild_and_search_stems(self):
        w = self._wiki([("order.service", "uses database", "PostgreSQL"),
                        ("billing.service", "uses database", "MySQL")])
        r = ftx.rebuild(w)
        self.assertEqual(r["rows"], 2)
        hits = ftx.search(w, "which databases do the order services use")
        self.assertIsNotNone(hits)
        ids = [h[0] for h in hits]
        self.assertTrue(all(i.startswith("claim_") for i in ids))
        rows = {c["id"]: c for c in claims.list_claims(w)}
        self.assertEqual(rows[ids[0]]["subject"], "order.service")

    def test_search_returns_none_when_stale(self):
        w = self._wiki([("a", "b", "c")])
        ftx.rebuild(w)
        st = w.state()
        st["revision"] += 1
        w.save_state(st)
        self.assertIsNone(ftx.search(w, "a b c"))
        h = ftx.health(w)
        self.assertFalse(h["usable"])
        self.assertEqual(h["reason"], "stale_revision")

    def test_search_returns_none_when_absent(self):
        w = self._wiki([])
        self.assertIsNone(ftx.search(w, "anything"))
        self.assertEqual(ftx.health(w)["reason"], "missing")

    def test_body_text_includes_scope_and_summary(self):
        body = ftx.body_text({"subject": "s", "predicate": "p", "value": "v",
                              "summary": "sum", "scope": {"env": "prod"}})
        self.assertIn("prod", body)
        self.assertIn("sum", body)
        self.assertIn("v", body)


if __name__ == "__main__":
    unittest.main()
