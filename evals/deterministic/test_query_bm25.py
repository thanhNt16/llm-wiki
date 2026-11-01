"""SP2a Task 5: query.prepare ranks via FTS BM25 when cache is live — TDD."""
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import claims, ftx, ids, query  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import Transaction  # noqa: E402


def _commit(w, subj, pred, val):
    txn = Transaction(w, "seed")
    claims.save_claim(txn, {"id": ids.new("claim"), "subject": subj,
                            "predicate": pred, "value": val,
                            "status": "accepted", "evidence": [],
                            "authority": {"type": "authoritative_source"}})
    txn.commit(w.revision())


class TestBm25Ranker(unittest.TestCase):
    def setUp(self):
        self.w = Wiki(tempfile.mkdtemp(prefix="ftx-rank-"))
        init_wiki(self.w, "ftx-rank")
        _commit(self.w, "rare.worker.01", "uses database", "Redis")
        _commit(self.w, "common.service.02", "uses database", "PostgreSQL")
        ftx.rebuild(self.w)

    def tearDown(self):
        shutil.rmtree(self.w.root, ignore_errors=True)

    def test_live_cache_ranks_by_bm25(self):
        r = query.prepare(self.w, "rare worker database")
        self.assertEqual(r["ranker"], "bm25")
        self.assertEqual(r["matches"][0]["subject"], "rare.worker.01")

    def test_unusable_cache_falls_back_to_legacy(self):
        os.remove(self.w.p(ftx.DB_REL))
        r = query.prepare(self.w, "rare worker database")
        self.assertEqual(r["ranker"], "legacy")
        self.assertTrue(r["matches"])  # still answers, just unweighted

    def test_superseded_claims_never_match_under_bm25(self):
        txn = Transaction(self.w, "sup")
        claims.save_claim(txn, {"id": ids.new("claim"),
                                "subject": "rare.worker.01",
                                "predicate": "uses database", "value": "Valkey",
                                "status": "superseded", "superseded_by": "claim_x",
                                "evidence": [],
                                "authority": {"type": "authoritative_source"}})
        txn.commit(self.w.revision())
        r = query.prepare(self.w, "Valkey")
        self.assertEqual(r["ranker"], "bm25")
        vals = [m.get("value") for m in r["matches"]]
        self.assertNotIn("Valkey", vals)
        self.assertTrue(any(s.get("value") == "Valkey" for s in r["superseded"]))


if __name__ == "__main__":
    unittest.main()
