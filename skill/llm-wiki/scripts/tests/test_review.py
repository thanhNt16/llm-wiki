import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, review, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import ConflictError, Transaction


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "review-test")
    return wiki


def seed_disputed(wiki):
    """Create a disputed claim + open contradiction review item via compile."""
    r1 = sources.ingest(wiki, "text", "one", b"claim one")
    c1 = {
        "id": "auto", "subject": "retries", "predicate": "count", "value": 3,
        "scope": {}, "valid_from": None, "valid_to": None,
        "recorded_at": "2026-09-15T00:00:00Z", "status": "candidate",
        "proposed_by": "agent", "authority": {"type": "agent_inference"},
        "evidence": [{"source_id": r1["source_id"], "source_version": 1,
                      "locator": {"type": "text", "value": "x"}}],
        "supersedes": [],
    }
    rid = wc_compile.stage_candidates(wiki, r1["source_id"], 1, [c1], {
        "source_id": r1["source_id"], "source_version": 1,
        "coverage": {"text": "complete"}, "warnings": [], "parser": {"name": "x"},
    })
    wc_compile.reconcile_apply(wiki, rid,
                               [{"index": 0, "relationship": "UNRELATED",
                                 "target_claim_id": None, "valid_from": None,
                                 "valid_to": None,
                                 "authority": {"type": "agent_inference"}}],
                               wiki.revision())
    target = claims.list_claims(wiki)[0]

    r2 = sources.ingest(wiki, "text", "two", b"claim two conflicts")
    c2 = dict(c1, id="auto", value=5)
    c2["evidence"] = [{"source_id": r2["source_id"], "source_version": 1,
                       "locator": {"type": "text", "value": "y"}}]
    rid2 = wc_compile.stage_candidates(wiki, r2["source_id"], 1, [c2], {
        "source_id": r2["source_id"], "source_version": 1,
        "coverage": {"text": "complete"}, "warnings": [], "parser": {"name": "x"},
    })
    rr = wc_compile.reconcile_apply(wiki, rid2,
                                    [{"index": 0, "relationship": "CONTRADICTION",
                                      "target_claim_id": target["id"],
                                      "valid_from": None, "valid_to": None,
                                      "authority": {"type": "agent_inference"}}],
                                    wiki.revision())
    item_id = rr["conflicts"][0]
    disputed = [c for c in claims.list_claims(wiki) if c["status"] == "disputed"][0]
    return item_id, target, disputed


class TestReview(unittest.TestCase):
    def test_accept_flips_claim_and_resolves_item(self):
        wiki = fresh()
        item_id, target, disputed = seed_disputed(wiki)
        out = review.act(wiki, item_id, "accept",
                         {"claim_id": disputed["id"], "note": "5 is correct"},
                         wiki.revision())
        self.assertEqual(out["item"]["status"], "resolved")
        got = claims.load_claim(wiki, disputed["id"])
        self.assertEqual(got["status"], "accepted")
        self.assertEqual(got["acceptance"]["reason"], "human_review")
        # the losing claim is untouched by accepting the disputed one
        self.assertEqual(claims.load_claim(wiki, target["id"])["status"], "candidate")

    def test_set_scope_updates_claim(self):
        wiki = fresh()
        item_id, target, disputed = seed_disputed(wiki)
        review.act(wiki, item_id, "set_scope",
                   {"claim_id": disputed["id"], "scope": {"environment": "staging"}},
                   wiki.revision())
        got = claims.load_claim(wiki, disputed["id"])
        self.assertEqual(got["scope"], {"environment": "staging"})

    def test_mark_superseded_links_target(self):
        wiki = fresh()
        item_id, target, disputed = seed_disputed(wiki)
        review.act(wiki, item_id, "mark_superseded",
                   {"claim_id": disputed["id"], "target": target["id"]},
                   wiki.revision())
        got = claims.load_claim(wiki, disputed["id"])
        self.assertEqual(got["status"], "superseded")
        self.assertIn(target["id"], got["supersedes"])

    def test_defer_keeps_claim_untouched(self):
        wiki = fresh()
        item_id, target, disputed = seed_disputed(wiki)
        review.act(wiki, item_id, "defer", {"note": "ask team"}, wiki.revision())
        items = review.list_items(wiki, status="deferred")
        self.assertEqual(len(items), 1)
        self.assertEqual(claims.load_claim(wiki, disputed["id"])["status"], "disputed")

    def test_stale_revision_rejected(self):
        wiki = fresh()
        item_id, target, disputed = seed_disputed(wiki)
        with self.assertRaises(ConflictError):
            review.act(wiki, item_id, "accept", {"claim_id": disputed["id"]},
                       wiki.revision() + 3)

    def test_unknown_action_rejected(self):
        wiki = fresh()
        item_id, target, disputed = seed_disputed(wiki)
        try:
            review.act(wiki, item_id, "make_it_so", {}, wiki.revision())
            self.fail("expected TxnError")
        except Exception as e:
            self.assertIn("action", str(e).lower())

    def test_list_filters_by_kind(self):
        wiki = fresh()
        item_id, target, disputed = seed_disputed(wiki)
        self.assertEqual(len(review.list_items(wiki, kind="possible_contradiction")), 1)
        self.assertEqual(review.list_items(wiki, kind="authority_conflict"), [])


if __name__ == "__main__":
    unittest.main()
