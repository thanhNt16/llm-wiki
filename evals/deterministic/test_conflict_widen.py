"""SP2a Task 6: contradiction detection must scan past the top-10 match
window (#8) — a conflicting claim ranked 11th+ is exactly the knowledge the
caller most needs (PRD §34). TDD: red on the truncated scan."""
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

SUBJ = "pay.service"


def _c(pred, val):
    return {"id": ids.new("claim"), "subject": SUBJ, "predicate": pred,
            "value": val, "status": "accepted", "evidence": [],
            "authority": {"type": "authoritative_source"}}


class TestConflictWidening(unittest.TestCase):
    def setUp(self):
        self.w = Wiki(tempfile.mkdtemp(prefix="ftx-conf-"))
        init_wiki(self.w, "ftx-conf")
        # contradiction pair: same subject/predicate/scope, different values
        self.mysql = _c("uses database", "mysql")
        self.mariadb = _c("uses database", "mariadb")
        # 10 fillers: same subject, distinct predicates. SP3-F1 migration:
        # under the unified _score ranker they must sit strictly between the
        # mysql twin (value match, 16) and the mariadb twin (no value match,
        # 13): non-query value (-3) + a scope token from the question (+1)
        # lands them at 14, so mysql stays in the window, mariadb below it.
        fillers = []
        for tag in ("alpha", "bravo", "charlie", "delta", "echo",
                    "foxtrot", "golf", "hotel", "india", "juliet"):
            f = _c("database use %s" % tag, "reservoir %s" % tag)
            f["scope"] = {"tier": "mysql"}
            fillers.append(f)
        txn = Transaction(self.w, "seed")
        for cl in [self.mysql, self.mariadb] + fillers:
            claims.save_claim(txn, cl)
        txn.commit(self.w.revision())
        ftx.rebuild(self.w)

    def tearDown(self):
        shutil.rmtree(self.w.root, ignore_errors=True)

    def test_contradiction_below_top10_still_surfaced(self):
        r = query.prepare(self.w, "which database does pay service use mysql")
        ranker_note = "(ranker=%s)" % r["ranker"]
        subjects = [m["subject"] for m in r["matches"]]
        self.assertEqual(len(r["matches"]), 10, ranker_note)  # window intact
        self.assertTrue(all(s == SUBJ for s in subjects), ranker_note)
        in_top = {m["value"] for m in r["matches"]}
        self.assertIn("mysql", in_top)
        self.assertNotIn("mariadb", in_top, ranker_note)  # twin is ranked below
        pairs = [c.get("claims", []) for c in r["conflicts"]]
        self.assertIn(sorted([self.mysql["id"], self.mariadb["id"]]),
                      [sorted(p) for p in pairs], ranker_note)


if __name__ == "__main__":
    unittest.main()
