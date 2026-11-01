"""SP2a Task 2: FTS cache coupled into Transaction.commit — TDD tests."""
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


def _claim(subj, pred, val):
    return {"id": ids.new("claim"), "subject": subj, "predicate": pred,
            "value": val, "status": "provisional", "evidence": [],
            "authority": {"type": "authoritative_source"}}


class TestTxnCoupling(unittest.TestCase):
    def setUp(self):
        self.w = Wiki(tempfile.mkdtemp(prefix="ftx-txn-"))
        init_wiki(self.w, "ftx-txn")

    def tearDown(self):
        shutil.rmtree(self.w.root, ignore_errors=True)

    def _commit_claim(self, subj, pred, val):
        txn = Transaction(self.w, "seed")
        claims.save_claim(txn, _claim(subj, pred, val))
        return txn.commit(self.w.revision())

    def test_commit_keeps_cache_current(self):
        self._commit_claim("svc.a", "uses database", "PostgreSQL")
        h = ftx.health(self.w)
        self.assertTrue(h["usable"], h)
        self.assertEqual(h["rows"], 1)
        self._commit_claim("svc.b", "uses database", "MySQL")
        h = ftx.health(self.w)
        self.assertTrue(h["usable"], h)
        self.assertEqual(h["rows"], 2)
        hits = ftx.search(self.w, "MySQL")
        self.assertEqual(len(hits), 1)             # only the patched row matches
        self.assertTrue(hits[0][0].startswith("claim_"))

    def test_stale_cache_is_rebuilt_on_commit(self):
        self._commit_claim("svc.a", "x", "y")
        os.remove(self.w.p(ftx.DB_REL))          # simulate loss
        self._commit_claim("svc.b", "x", "z")    # rebuild path
        self.assertTrue(ftx.health(self.w)["usable"])
        self.assertEqual(ftx.health(self.w)["rows"], 2)

    def test_commit_without_claims_keeps_cache_usable(self):
        self._commit_claim("svc.a", "x", "y")
        txn = Transaction(self.w, "no-claims")
        txn.commit(self.w.revision())            # e.g. decision-only commit
        # claim content is unchanged by this commit; the stamp must advance
        # with the manifest so readers do not silently fall back
        self.assertTrue(ftx.health(self.w)["usable"])
        self.assertEqual(ftx.health(self.w)["rows"], 1)


if __name__ == "__main__":
    unittest.main()
