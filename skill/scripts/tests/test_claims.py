import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError

SID = "source_" + "A" * 26


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "claims-test")
    return wiki


def claim(wiki, subject="s", predicate="p", value=1, status="candidate",
          evidence_sources=((SID, 1),), root_origins=None, **kw):
    evidence = []
    for i, (sid, ver) in enumerate(evidence_sources):
        loc = {"type": "text", "value": "loc-%d" % i}
        evidence.append({"source_id": sid, "source_version": ver, "locator": loc})
    c = {
        "id": "claim_" + claims.ids.new("claim").split("_", 1)[1],
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "scope": kw.get("scope", {}),
        "valid_from": kw.get("valid_from"),
        "valid_to": kw.get("valid_to"),
        "recorded_at": "2026-09-15T00:00:00Z",
        "status": status,
        "authority": kw.get("authority", {"type": "agent_inference"}),
        "evidence": evidence,
        "supersedes": kw.get("supersedes", []),
        "version": 1,
    }
    if root_origins:
        c["root_origin"] = root_origins[0]
        for ev, ro in zip(c["evidence"], root_origins):
            ev["root_origin"] = ro
    return c


class TestClaims(unittest.TestCase):
    def test_save_load_round_trip(self):
        wiki = fresh()
        txn = Transaction(wiki, "test")
        c = claim(wiki)
        claims.save_claim(txn, c)
        txn.commit(0)
        loaded = claims.load_claim(wiki, c["id"])
        self.assertEqual(loaded["subject"], "s")

    def test_schema_violation_rejected(self):
        wiki = fresh()
        txn = Transaction(wiki, "test")
        c = claim(wiki)
        del c["subject"]
        with self.assertRaises(TxnError):
            claims.save_claim(txn, c)

    def test_version_bump_on_update(self):
        wiki = fresh()
        txn = Transaction(wiki, "t1")
        c = claim(wiki)
        claims.save_claim(txn, c)
        txn.commit(0)
        txn2 = Transaction(wiki, "t2")
        c2 = claims.load_claim(wiki, c["id"])
        c2["value"] = 42
        claims.save_claim(txn2, c2)
        txn2.commit(1)
        self.assertEqual(claims.load_claim(wiki, c["id"])["version"], 2)
        self.assertEqual(claims.load_claim(wiki, c["id"])["value"], 42)

    def test_list_filters(self):
        wiki = fresh()
        txn = Transaction(wiki, "t")
        claims.save_claim(txn, claim(wiki, subject="a", status="accepted"))
        claims.save_claim(txn, claim(wiki, subject="b", status="candidate"))
        txn.commit(0)
        self.assertEqual(len(claims.list_claims(wiki, status="accepted")), 1)
        self.assertEqual(claims.list_claims(wiki, subject="b")[0]["status"], "candidate")
        self.assertEqual(len(claims.list_claims(wiki)), 2)

    def test_supersede_correction(self):
        wiki = fresh()
        txn = Transaction(wiki, "t")
        old = claim(wiki, status="accepted")
        claims.save_claim(txn, old)
        txn.commit(0)
        txn2 = Transaction(wiki, "t2")
        old_loaded = claims.load_claim(wiki, old["id"])
        updated = claims.supersede(txn2, old_loaded, "claim_NEW", policy_change=False,
                                   new_valid_from=None)
        claims.save_claim(txn2, updated)
        txn2.commit(1)
        got = claims.load_claim(wiki, old["id"])
        self.assertEqual(got["status"], "superseded")
        self.assertEqual(got["superseded_by"], "claim_NEW")
        self.assertIsNone(got.get("valid_to"))

    def test_supersede_policy_change_sets_valid_to(self):
        wiki = fresh()
        txn = Transaction(wiki, "t")
        old = claim(wiki, status="accepted")
        claims.save_claim(txn, old)
        txn.commit(0)
        txn2 = Transaction(wiki, "t2")
        updated = claims.supersede(txn2, claims.load_claim(wiki, old["id"]), "claim_NEW",
                                   policy_change=True, new_valid_from="2026-09-01")
        claims.save_claim(txn2, updated)
        txn2.commit(1)
        self.assertEqual(claims.load_claim(wiki, old["id"])["valid_to"], "2026-08-31")

    def test_independence_counts_distinct_origins(self):
        wiki = fresh()
        c = claim(wiki,
                  evidence_sources=((SID, 1), ("source_" + "B" * 26, 1),
                                    ("source_" + "C" * 26, 2), ("source_" + "B" * 26, 1)),
                  root_origins=[SID, "source_" + "B" * 26, "source_" + "C" * 26,
                                "source_" + "B" * 26])
        self.assertEqual(claims.independence(wiki, c), 3)
        # 4 references but 3 independent origins (PRD §7)
        self.assertEqual(len(c["evidence"]), 4)


class TestDecisions(unittest.TestCase):
    def test_decision_round_trip(self):
        wiki = fresh()
        d = {
            "id": "decision_" + claims.ids.new("decision").split("_", 1)[1],
            "title": "Use PostgreSQL",
            "status": "accepted",
            "decided_on": "2026-01-10",
            "recorded_at": "2026-09-15T00:00:00Z",
            "version": 1,
            "claims": [],
            "evidence": [{
                "source_id": SID, "source_version": 1,
                "locator": {"type": "heading", "value": "Decision"},
            }],
            "body": "We chose Postgres over DynamoDB.",
        }
        txn = Transaction(wiki, "t")
        claims.save_decision(txn, d)
        txn.commit(0)
        self.assertEqual(claims.load_decision(wiki, d["id"])["title"], "Use PostgreSQL")
        self.assertEqual(len(claims.list_decisions(wiki)), 1)


if __name__ == "__main__":
    unittest.main()
