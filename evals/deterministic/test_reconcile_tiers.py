"""SP3-T3 (#7): deterministic subject-key tiers for reconciliation.

Today reconcile_prepare matches candidates to existing claims by ONE
normalized key (subject+predicate, strip+lower). A candidate like
``order_service`` never sees the existing claim ``order.service`` and is
auto-classified UNRELATED -> silently merged as a brand-new claim.

Tier design (spec #7):
- t1 = today's exact normalized subject (strip + lowercase)
- t2 = lowercase + strip ``[._/\\- ]`` separators
- t3 = t2 + plural fold (trailing s/es stripped, min stem length 3)

An exact t1 match keeps today's classification path. A t2/t3-only match
must NEVER auto-merge: apply appends a ``needs_review`` item and the
candidate is classified ``rejected_tier_match`` with no canonical write.
"""
import json
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
sys.path.insert(0, os.path.join(REPO, "skill", "llm-wiki", "scripts"))

from wikicore import claims  # noqa: E402,F401 -- first: breaks the
# transaction<->ftx<->claims import cycle
from wikicore import compile as wc_compile, review, sources  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402


def fresh():
    wiki = Wiki(tempfile.mkdtemp())
    init_wiki(wiki, "tier-test")
    return wiki


def ingest(wiki, ref, body):
    return sources.ingest(wiki, "text", ref, body.encode("utf-8"))


REPORT = {"coverage": {"text": "complete"}, "warnings": [], "parser": {}}


def cand(subject, predicate, value, locator="s:fixture"):
    """Shorthand candidate — normalized at staging (no unknown keys)."""
    return {"subject": subject, "predicate": predicate, "value": value,
            "locator": locator, "authority": "doc"}


def compile_one(wiki, receipt, candidates, classifications=None):
    """stage -> prepare -> apply in one shot; returns apply receipt."""
    rid = wc_compile.stage_candidates(
        wiki, receipt["source_id"], receipt["version"], candidates, REPORT)
    wc_compile.reconcile_prepare(wiki, rid)
    return wc_compile.reconcile_apply(wiki, rid, classifications or [],
                                      wiki.revision())


def seed_claim(wiki, subject, predicate, value):
    """Compile one claim into the store; returns the claim dict."""
    r = ingest(wiki, "seed", "seed content")
    compile_one(wiki, r, [cand(subject, predicate, value)])
    found = [c for c in claims.list_claims(wiki)
             if c["subject"] == subject and c["predicate"] == predicate]
    assert found, "seed claim missing"
    return found[0]


def claims_named(wiki, subject):
    return [c for c in claims.list_claims(wiki) if c["subject"] == subject]


class TestTierKeys(unittest.TestCase):
    def test_exact_key_unchanged(self):
        t1, t2, t3 = wc_compile._tier_keys("order.service")
        self.assertEqual(t1, "order.service")
        self.assertEqual(t2, "orderservice")
        # 'order'/'service' are already singular — no fold
        self.assertEqual(t3, "orderservice")

    def test_separator_and_case_fold(self):
        _, t2, _ = wc_compile._tier_keys("Order_Service / billing")
        self.assertEqual(t2, "orderservicebilling")

    def test_plural_fold_min_stem(self):
        _, _, t3 = wc_compile._tier_keys("orders.service")
        self.assertEqual(t3, "orderservice")
        # stems shorter than 3 chars do not fold ('ids' -> 'id' len 2)
        _, _, t3 = wc_compile._tier_keys("api.ids")
        self.assertEqual(t3, "apiids")


class TestTierReconcile(unittest.TestCase):
    def test_exact_match_still_auto(self):
        # regression guard: t1 matches keep the old auto-verdict path —
        # same origin restates the fact -> DUPLICATE, no review item
        seed = seed_claim(wiki := fresh(), "order.service", "ttl", 300)
        r2 = sources.ingest(wiki, "text", "seed", b"seed v2", force=True)
        receipt = compile_one(
            wiki, r2, [cand("order.service", "ttl", 300)])
        self.assertEqual(receipt["auto"]["duplicate"], 1)
        self.assertEqual(receipt["changes"]["claims_created"], 0)
        self.assertEqual(len(claims_named(wiki, "order.service")), 1)
        tier_items = [i for i in review.list_items(wiki)
                      if "tier" in i["problem"]]
        self.assertEqual(tier_items, [])
        self.assertEqual(seed["subject"], "order.service")

    def test_tier2_mismatch_goes_to_review(self):
        # order.service exists; order_service folds to the same t2 key
        # but must NOT auto-merge — review item + no canonical write
        seed_claim(wiki := fresh(), "order.service", "ttl", 300)
        r = ingest(wiki, "s2", "second source")
        rid = wc_compile.stage_candidates(
            wiki, r["source_id"], r["version"],
            [cand("order_service", "ttl", 300)], REPORT)
        prep = wc_compile.reconcile_prepare(wiki, rid)
        row = prep["comparisons"][0]
        self.assertEqual(row["auto"], "rejected_tier_match")
        self.assertEqual(row["matches"][0]["tier"], 2)
        receipt = wc_compile.reconcile_apply(wiki, rid, [], wiki.revision())
        self.assertEqual(receipt["auto"]["rejected_tier_match"], 1)
        self.assertEqual(receipt["changes"]["claims_created"], 0)
        self.assertEqual(receipt["changes"]["claims_updated"], 0)
        self.assertEqual(claims_named(wiki, "order_service"), [])
        self.assertEqual(receipt["changes"]["review_items"], 1)
        items = [i for i in review.list_items(wiki)
                 if "tier" in i["problem"]]
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0]["kind"], "needs_review")
        self.assertIn("tier-2 normalization match", items[0]["problem"])
        self.assertIn("order_service", items[0]["problem"])
        self.assertIn("order.service", items[0]["problem"])
        self.assertIn("agent adjudication required", items[0]["problem"])

    def test_tier3_plural_fold_goes_to_review(self):
        # the brief's example: orders_service -> t3 'orderservice' ==
        # order.service's t3; t2 differs ('ordersservice' !=
        # 'orderservice') -> tier 3 route
        seed_claim(wiki := fresh(), "order.service", "ttl", 300)
        r = ingest(wiki, "s2", "second source")
        rid = wc_compile.stage_candidates(
            wiki, r["source_id"], r["version"],
            [cand("orders_service", "ttl", 300)], REPORT)
        prep = wc_compile.reconcile_prepare(wiki, rid)
        row = prep["comparisons"][0]
        self.assertEqual(row["auto"], "rejected_tier_match")
        self.assertEqual(row["matches"][0]["tier"], 3)
        receipt = wc_compile.reconcile_apply(wiki, rid, [], wiki.revision())
        self.assertEqual(receipt["changes"]["claims_created"], 0)
        self.assertEqual(claims_named(wiki, "orders_service"), [])
        items = [i for i in review.list_items(wiki)
                 if "tier" in i["problem"]]
        self.assertEqual(len(items), 1)
        self.assertIn("tier-3 normalization match", items[0]["problem"])
        self.assertIn("orders_service", items[0]["problem"])
        self.assertIn("order.service", items[0]["problem"])

    def test_different_predicate_still_unrelated(self):
        # tiers fold the SUBJECT only — same folded subject under a
        # different predicate must not collide
        seed_claim(wiki := fresh(), "order.service", "ttl", 300)
        r = ingest(wiki, "s2", "second source")
        receipt = compile_one(
            wiki, r, [cand("orders_service", "max_retries", 3)])
        self.assertEqual(receipt["auto"]["unrelated"], 1)
        self.assertEqual(receipt["changes"]["claims_created"], 1)
        self.assertEqual(len(claims_named(wiki, "orders_service")), 1)

    def test_agent_override_can_still_add_claim(self):
        # the tier route produces a review item; an explicit agent
        # classification (adjudication) may still add the claim — the
        # invariant is only that it never AUTO-merges
        seed_claim(wiki := fresh(), "order.service", "ttl", 300)
        r = ingest(wiki, "s2", "second source")
        receipt = compile_one(
            wiki, r, [cand("orders_service", "ttl", 300)],
            classifications=[{"index": 0, "relationship": "UNRELATED"}])
        self.assertEqual(receipt["changes"]["claims_created"], 1)
        self.assertEqual(len(claims_named(wiki, "orders_service")), 1)


if __name__ == "__main__":
    unittest.main()
