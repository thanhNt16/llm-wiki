import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, deps, query, sources
from wikicore.hashing import sha256_file
from wikicore.store import Wiki, init_wiki

def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "query-test")
    return wiki

def add_claim(wiki, ref, subject, predicate, value, valid_from=None, valid_to=None,
              status_authority="explicit_project_decision"):
    r = sources.ingest(wiki, "text", ref, ("body %s" % ref).encode())
    cand = {
        "id": "auto", "subject": subject, "predicate": predicate, "value": value,
        "scope": {"environment": "production"}, "valid_from": valid_from,
        "valid_to": valid_to, "recorded_at": "2026-09-15T00:00:00Z",
        "status": "candidate", "proposed_by": "agent",
        "authority": {"type": status_authority},
        "evidence": [{"source_id": r["source_id"], "source_version": r["version"],
                      "locator": {"type": "heading", "value": "H"}}],
        "supersedes": [],
    }
    run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"], [cand], {
        "source_id": r["source_id"], "source_version": r["version"],
        "coverage": {"text": "complete"}, "warnings": [], "parser": {"name": "x"},
    })
    wc_compile.reconcile_apply(wiki, run_id,
                               [{"index": 0, "relationship": "UNRELATED",
                                 "target_claim_id": None, "valid_from": valid_from,
                                 "valid_to": valid_to,
                                 "authority": {"type": status_authority}}],
                               wiki.revision())

class TestQuery(unittest.TestCase):
    def _seed_temporal(self):
        wiki = fresh()
        add_claim(wiki, "old-adr", "analytics", "window", 30,
                  valid_from="2026-01-01", valid_to="2026-08-31")
        add_claim(wiki, "memo", "analytics", "window", 7, valid_from="2026-09-01")
        return wiki

    def test_current_query_excludes_superseded(self):
        wiki = self._seed_temporal()
        out = query.prepare(wiki, "what is the analytics window?")
        ids_list = [m["claim_id"] for m in out["matches"]]
        statuses = [m["status"] for m in out["matches"]]
        self.assertNotIn("superseded", statuses)
        current = [m for m in out["matches"] if m["value"] == 7]
        self.assertEqual(len(current), 1)

    def test_historical_as_of_returns_old_value(self):
        wiki = self._seed_temporal()
        out = query.prepare(wiki, "what was the analytics window?", as_of="2026-08-15")
        values = [m["value"] for m in out["matches"]]
        self.assertIn(30, values)
        self.assertNotIn(7, values)

    def test_conflicts_surfaced(self):
        wiki = fresh()
        add_claim(wiki, "a", "retries", "count", 3, status_authority="agent_inference")
        add_claim(wiki, "b", "retries", "count", 5, status_authority="agent_inference")
        out = query.prepare(wiki, "how many retries are configured?")
        self.assertGreaterEqual(len(out["conflicts"]), 1)

    def test_gaps_listed_for_unknown_topic(self):
        wiki = fresh()
        out = query.prepare(wiki, "what is our 2028 pricing plan?")
        self.assertEqual(out["matches"], [])
        self.assertTrue(any("pricing" in g or "2028" in g for g in out["gaps"]))

    def test_query_is_read_only(self):
        wiki = self._seed_temporal()

        def snapshot():
            state = {}
            for dirpath, _dirnames, filenames in os.walk(wiki.dot):
                for fn in filenames:
                    p = os.path.join(dirpath, fn)
                    state[p] = sha256_file(p)
            return state

        before = snapshot()
        query.prepare(wiki, "analytics window?")
        after = snapshot()
        self.assertEqual(before, after)

    def test_evidence_refs_included(self):
        wiki = self._seed_temporal()
        out = query.prepare(wiki, "analytics window?")
        m = out["matches"][0]
        self.assertTrue(m["evidence"])
        self.assertIn("source_id", m["evidence"][0])

    def test_score_breakdown_exposed(self):
        wiki = fresh()
        add_claim(wiki, "w1", "analytics", "window", 7)
        out = query.prepare(wiki, "analytics window?")
        m = out["matches"][0]
        self.assertEqual(m["score"], m["score_breakdown"]["subject"] * 3
                         + m["score_breakdown"]["predicate"] * 2
                         + m["score_breakdown"]["value"]
                         + m["score_breakdown"]["status"]
                         + m["score_breakdown"]["evidence"]
                         + m["score_breakdown"]["scope"])
        self.assertGreater(m["score_breakdown"]["subject"], 0)

def put_claim_json(wiki, cid, subject, predicate, value, status="accepted", **extra):
    import json
    claim = {"id": cid, "subject": subject, "predicate": predicate, "value": value,
             "scope": {}, "valid_from": None, "valid_to": None,
             "recorded_at": "2026-09-15T00:00:00Z", "status": status,
             "proposed_by": "agent",
             "authority": {"type": "explicit_project_decision"},
             "evidence": [{"source_id": "src_x", "source_version": 1,
                           "locator": {"type": "text", "value": "x"}}],
             "supersedes": [], "version": 1}
    claim.update(extra)
    os.makedirs(wiki.p("claims"), exist_ok=True)
    with open(wiki.p("claims/%s.json" % cid), "w") as f:
        json.dump(claim, f)

class TestMorphology(unittest.TestCase):
    def test_inflected_question_matches_singular_claim(self):
        # raw tokens "payments"/"retained" would never hit "payment"/"retention";
        # norm_tokens stems both sides (Q1 acceptance)
        wiki = fresh()
        put_claim_json(wiki, "claim_p", "payments.retention", "retention_period",
                       "365 days")
        out = query.prepare(wiki, "how long are payment events retained?")
        self.assertEqual([m["claim_id"] for m in out["matches"]], ["claim_p"])
        self.assertGreaterEqual(out["matches"][0]["score_breakdown"]["subject"], 1)

class TestSupersededVisibility(unittest.TestCase):
    def test_superseded_only_question_returns_stale_gap(self):
        wiki = fresh()
        put_claim_json(wiki, "claim_old", "cache", "ttl", 60,
                       status="superseded", superseded_by="claim_new")
        put_claim_json(wiki, "claim_new", "apirate", "limit", 999)
        out = query.prepare(wiki, "cache ttl seconds?")
        self.assertEqual(out["matches"], [])
        sup = out["superseded"]
        self.assertEqual(len(sup), 1)
        self.assertEqual(sup[0]["claim_id"], "claim_old")
        self.assertEqual(sup[0]["replaced_by"], "claim_new")
        self.assertIn("stale_knowledge", out["gaps"])

class TestReviewLinkage(unittest.TestCase):
    def test_review_item_surfaces_for_sibling_claim(self):
        from wikicore import review
        from wikicore.transaction import Transaction
        wiki = fresh()
        add_claim(wiki, "b1", "billing", "cycle", "monthly")
        add_claim(wiki, "b2", "billing", "currency", "usd")
        claims_list = claims.list_claims(wiki)
        target = next(c for c in claims_list if c["predicate"] == "currency")
        txn = Transaction(wiki, "test-review")
        review.add_item(txn, "contradiction", "currency might change to eur",
                        [], ["%s@%d" % (target["id"], target["version"])], "low")
        txn.commit(wiki.revision())
        # query matches the SIBLING (cycle), not the affected claim itself
        out = query.prepare(wiki, "billing cycle frequency?")
        self.assertTrue(any(c.get("review_id") for c in out["conflicts"]))

class TestSubjects(unittest.TestCase):
    def test_subjects_rollup(self):
        wiki = fresh()
        add_claim(wiki, "r1", "billing", "cycle", "monthly")
        add_claim(wiki, "r2", "billing", "amount", 42)
        add_claim(wiki, "r3", "shipping", "zone", "eu")
        out = query.subjects(wiki)
        by = {s["subject"]: s for s in out}
        b = by["billing"]
        self.assertEqual(b["predicates"], ["amount", "cycle"])
        self.assertEqual(b["claim_count"], 2)
        self.assertEqual(b["statuses"], {"accepted": 2})
        self.assertGreaterEqual(b["source_count"], 1)
        self.assertEqual(by["shipping"]["claim_count"], 1)

    def test_subjects_prefix_and_stale_pages(self):
        wiki = fresh()
        add_claim(wiki, "r1", "billing", "cycle", "monthly")
        c = claims.list_claims(wiki)[0]
        os.makedirs(wiki.p("wiki/concepts"), exist_ok=True)
        with open(wiki.p("wiki/concepts/billing.md"), "w") as f:
            f.write("---\ntype: concept\nsubject: billing\ntitle: Billing\n"
                    "created: 2026-09-15T00:00:00Z\ndeps:\n  - %s@%d\nstale: true\n---\n\nx\n"
                    % (c["id"], c["version"]))
        out = query.subjects(wiki, prefix="bill")
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0]["stale_pages"], ["wiki/concepts/billing.md"])

if __name__ == "__main__":
    unittest.main()
