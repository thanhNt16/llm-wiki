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


if __name__ == "__main__":
    unittest.main()
