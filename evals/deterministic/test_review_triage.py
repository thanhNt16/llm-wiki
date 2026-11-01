"""SP3-T6 (#14): deterministic review-queue triage + transactional batch apply.

``review.triage`` groups open queue items (default key: subject of the
linked claim) and attaches a conservative suggested action:
  - "reject"  iff problem text contains "injection" or "secret" AND a
    claim is linked (claim-less suspicious items defer instead — reject
    requires claim_id, so emitting it would abort a verbatim apply_batch)
  - "defer"   otherwise

``review.apply_batch`` executes a list of {id, action, params} inside ONE
Transaction — a single commit, a single revision bump, atomic abort on any
failure — against a caller-supplied base_revision (stale => ConflictError).
"""
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
from wikicore.transaction import ConflictError, Transaction, TxnError  # noqa: E402


def fresh():
    wiki = Wiki(tempfile.mkdtemp())
    init_wiki(wiki, "triage-test")
    return wiki


def ingest(wiki, ref, body):
    return sources.ingest(wiki, "text", ref, body.encode("utf-8"))


REPORT = {"coverage": {"text": "complete"}, "warnings": [], "parser": {}}


def cand(subject, predicate, value, locator="s:fixture", authority="doc"):
    """Shorthand candidate — normalized at staging (no unknown keys)."""
    return {"subject": subject, "predicate": predicate, "value": value,
            "locator": locator, "authority": authority}


def compile_one(wiki, receipt, candidates, classifications=None):
    rid = wc_compile.stage_candidates(
        wiki, receipt["source_id"], receipt["version"], candidates, REPORT)
    wc_compile.reconcile_prepare(wiki, rid)
    return wc_compile.reconcile_apply(wiki, rid, classifications or [],
                                      wiki.revision())


def seed_claim(wiki, subject, predicate, value, origins=1):
    """Compile one claim; `origins` independent sources restate it so the
    stored claim carries that many distinct evidence origins."""
    for n in range(origins):
        r = ingest(wiki, "seed-%s-%d" % (subject, n), "seed body %d" % n)
        compile_one(wiki, r, [cand(subject, predicate, value)])
    found = [c for c in claims.list_claims(wiki)
             if c["subject"] == subject and c["predicate"] == predicate]
    assert found, "seed claim missing"
    return found[0]


def seed_candidate(wiki, subject, predicate, value):
    """Compile one claim with agent_inference authority — stays
    'candidate' (below the auto-accept bar)."""
    r = ingest(wiki, "cand-%s" % subject, "candidate body")
    compile_one(wiki, r, [cand(subject, predicate, value,
                               authority={"type": "agent_inference"})])
    found = [c for c in claims.list_claims(wiki)
             if c["subject"] == subject and c["predicate"] == predicate]
    assert found and found[0]["status"] == "candidate", "seed not candidate"
    return found[0]


def tier_item(wiki, cand_subject, source_ref):
    """Compile a t2/t3-only candidate -> needs_review item; returns item id."""
    r = ingest(wiki, source_ref, "tier body %s" % source_ref)
    rid = wc_compile.stage_candidates(
        wiki, r["source_id"], r["version"], [cand(cand_subject, "ttl", 300)],
        REPORT)
    wc_compile.reconcile_prepare(wiki, rid)
    receipt = wc_compile.reconcile_apply(wiki, rid, [], wiki.revision())
    assert receipt["changes"]["review_items"] == 1
    return receipt["conflicts"][0]


def add_direct(wiki, kind, problem):
    """Seed a queue item directly through add_item's schema-validated path."""
    txn = Transaction(wiki, "wiki-review")
    item_id = review.add_item(txn, kind, problem, [], [], "medium")
    txn.commit(wiki.revision())
    return item_id


def subject_of(wiki, claim):
    return claim["subject"]


def add_with_claim(wiki, kind, problem, claim_id):
    """Queue item carrying a linked claim id in affected."""
    txn = Transaction(wiki, "wiki-review")
    item_id = review.add_item(txn, kind, problem, [], [claim_id], "medium")
    txn.commit(wiki.revision())
    return item_id


class TestTriageGrouping(unittest.TestCase):
    def test_groups_by_subject_sorted(self):
        wiki = fresh()
        seed_claim(wiki, "order.service", "ttl", 300)
        seed_claim(wiki, "api.gateway", "ttl", 300)
        i1 = tier_item(wiki, "order_service", "t1")
        i2 = tier_item(wiki, "orders_service", "t2")  # t3 fold, same subject
        i3 = tier_item(wiki, "api_gateway", "t3")
        out = review.triage(wiki)
        keys = [g["key"] for g in out["groups"]]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(keys, ["api.gateway", "order.service"])
        order = next(g for g in out["groups"] if g["key"] == "order.service")
        self.assertEqual(order["count"], 2)
        self.assertEqual({i["id"] for i in order["items"]}, {i1, i2})
        api = next(g for g in out["groups"] if g["key"] == "api.gateway")
        self.assertEqual(api["count"], 1)
        self.assertEqual(api["items"][0]["id"], i3)
        self.assertEqual(api["items"][0]["subject"], "api.gateway")

    def test_group_by_kind(self):
        wiki = fresh()
        add_direct(wiki, "ambiguous_scope", "scope unclear for page Z")
        seed_claim(wiki, "order.service", "ttl", 300)
        tier_item(wiki, "order_service", "t1")
        out = review.triage(wiki, group_by="kind")
        keys = [g["key"] for g in out["groups"]]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(keys, ["ambiguous_scope", "needs_review"])


class TestSuggestions(unittest.TestCase):
    def test_needs_review_two_origins_suggests_accept(self):
        wiki = fresh()
        seed = seed_claim(wiki, "order.service", "ttl", 300, origins=2)
        self.assertGreaterEqual(claims.independence(wiki, seed), 2)
        item_id = tier_item(wiki, "order_service", "t1")
        out = review.triage(wiki)
        row = out["groups"][0]["items"][0]
        self.assertEqual(row["id"], item_id)
        self.assertEqual(row["kind"], "needs_review")
        self.assertEqual(row["suggested_action"], "accept")
        self.assertEqual(row["params"], {"claim_id": seed["id"]})

    def test_needs_review_one_origin_defers(self):
        wiki = fresh()
        seed_claim(wiki, "beta.service", "ttl", 300, origins=1)
        tier_item(wiki, "beta_service", "t1")
        row = review.triage(wiki)["groups"][0]["items"][0]
        self.assertEqual(row["suggested_action"], "defer")
        self.assertEqual(row["params"], {})

    def test_suspicious_text_defers_claimless_rejects_with_claim(self):
        wiki = fresh()
        # claim-less items can't be auto-rejected (reject requires
        # claim_id): triage emits defer so verbatim apply_batch survives
        add_direct(wiki, "ambiguous_scope", "possible secret leak in docs")
        add_direct(wiki, "low_extraction_quality",
                   "candidate contains injection attempt")
        # the same text WITH a linked claim still suggests reject
        seed = seed_claim(wiki, "evil.service", "ttl", 300)
        add_with_claim(wiki, "needs_review",
                       "injection attempt in evidence", seed["id"])
        out = review.triage(wiki)
        rows = {r["id"]: r for g in out["groups"] for r in g["items"]}
        by_kind = {r["kind"]: r["suggested_action"] for r in rows.values()}
        self.assertEqual(by_kind["ambiguous_scope"], "defer")
        self.assertEqual(by_kind["low_extraction_quality"], "defer")
        self.assertEqual(by_kind["needs_review"], "reject")
        linked = next(r for r in rows.values()
                      if r["kind"] == "needs_review")
        self.assertEqual(linked["params"], {"claim_id": seed["id"]})

    def test_benign_item_defers(self):
        wiki = fresh()
        add_direct(wiki, "ambiguous_scope", "scope unclear for page Z")
        row = review.triage(wiki)["groups"][0]["items"][0]
        self.assertEqual(row["suggested_action"], "defer")
        self.assertEqual(row["params"], {})


class TestApplyBatch(unittest.TestCase):
    def test_batch_defers_two_items_in_one_commit(self):
        wiki = fresh()
        seed_claim(wiki, "order.service", "ttl", 300)
        i1 = tier_item(wiki, "order_service", "t1")
        i2 = tier_item(wiki, "orders_service", "t2")
        rev = wiki.revision()
        out = review.apply_batch(wiki, [
            {"id": i1, "action": "defer", "params": {}},
            {"id": i2, "action": "defer", "params": {"note": "later"}},
        ], rev)
        self.assertEqual(out["applied"], 2)
        self.assertEqual(len(out["results"]), 2)
        # ONE transaction => exactly one revision bump
        self.assertEqual(wiki.revision(), rev + 1)
        self.assertEqual(review.show(wiki, i1)["status"], "deferred")
        self.assertEqual(review.show(wiki, i2)["status"], "deferred")
        self.assertEqual(review.list_items(wiki), [])
        self.assertEqual(review.show(wiki, i2)["history"][-1]["note"], "later")

    def test_batch_accept_mutates_claim_same_commit(self):
        wiki = fresh()
        seed = seed_claim(wiki, "order.service", "ttl", 300, origins=2)
        item_id = tier_item(wiki, "order_service", "t1")
        rev = wiki.revision()
        out = review.apply_batch(wiki, [
            {"id": item_id, "action": "accept",
             "params": {"claim_id": seed["id"]}},
        ], rev)
        self.assertEqual(out["applied"], 1)
        self.assertEqual(wiki.revision(), rev + 1)
        self.assertEqual(review.show(wiki, item_id)["status"], "resolved")
        self.assertEqual(claims.load_claim(wiki, seed["id"])["status"],
                         "accepted")

    def test_stale_base_revision_rejected(self):
        wiki = fresh()
        seed_claim(wiki, "order.service", "ttl", 300)
        item_id = tier_item(wiki, "order_service", "t1")
        with self.assertRaises(ConflictError):
            review.apply_batch(wiki, [
                {"id": item_id, "action": "defer", "params": {}},
            ], wiki.revision() + 3)
        self.assertEqual(review.show(wiki, item_id)["status"], "open")

    def test_batch_abort_is_atomic(self):
        """A failing action mid-batch must roll back earlier items too."""
        wiki = fresh()
        seed_claim(wiki, "order.service", "ttl", 300)
        i1 = tier_item(wiki, "order_service", "t1")
        rev = wiki.revision()
        with self.assertRaises(TxnError):
            review.apply_batch(wiki, [
                {"id": i1, "action": "defer", "params": {}},
                {"id": "review_doesnotexist", "action": "defer", "params": {}},
            ], rev)
        self.assertEqual(wiki.revision(), rev)
        self.assertEqual(review.show(wiki, i1)["status"], "open")

    def test_unknown_action_rejected(self):
        wiki = fresh()
        seed_claim(wiki, "order.service", "ttl", 300)
        item_id = tier_item(wiki, "order_service", "t1")
        with self.assertRaises(TxnError):
            review.apply_batch(wiki, [
                {"id": item_id, "action": "make_it_so", "params": {}},
            ], wiki.revision())



class TestBatchStagedClaimReads(unittest.TestCase):
    """SP3-F3: batch actions on the SAME claim must see each other's
    staged mutations — sequential act() semantics inside one commit."""

    def test_accept_then_set_validity_no_lost_update(self):
        """[accept, set_validity] on one claim: the accept must survive.
        Pre-fix the second action re-read live-disk bytes, so the staged
        accept was silently overwritten (status stayed 'candidate')."""
        wiki = fresh()
        seed = seed_candidate(wiki, "order.service", "ttl", 300)
        i1 = add_with_claim(wiki, "needs_review", "adj 1", seed["id"])
        i2 = add_with_claim(wiki, "needs_review", "adj 2", seed["id"])
        out = review.apply_batch(wiki, [
            {"id": i1, "action": "accept", "params": {"claim_id": seed["id"]}},
            {"id": i2, "action": "set_validity",
             "params": {"claim_id": seed["id"], "valid_from": "2026-01-01"}},
        ], wiki.revision())
        self.assertEqual(out["applied"], 2)
        got = claims.load_claim(wiki, seed["id"])
        self.assertEqual(got["status"], "accepted")
        self.assertEqual(got["acceptance"]["reason"], "human_review")
        self.assertEqual(got["valid_from"], "2026-01-01")
        self.assertEqual(got["version"], seed["version"] + 1)
        self.assertEqual(review.show(wiki, i1)["status"], "resolved")
        self.assertEqual(review.show(wiki, i2)["status"], "resolved")

    def test_reject_then_accept_hits_terminal_guard(self):
        """[reject, accept] on one claim must raise the terminal-status
        TxnError that sequential act() enforces — not bypass it."""
        wiki = fresh()
        seed = seed_candidate(wiki, "order.service", "ttl", 300)
        i1 = add_with_claim(wiki, "needs_review", "adj 1", seed["id"])
        i2 = add_with_claim(wiki, "needs_review", "adj 2", seed["id"])
        rev = wiki.revision()
        with self.assertRaises(TxnError) as ctx:
            review.apply_batch(wiki, [
                {"id": i1, "action": "reject",
                 "params": {"claim_id": seed["id"]}},
                {"id": i2, "action": "accept",
                 "params": {"claim_id": seed["id"]}},
            ], rev)
        self.assertIn("cannot return", str(ctx.exception))
        # atomic abort: nothing staged leaks through
        self.assertEqual(wiki.revision(), rev)
        self.assertEqual(claims.load_claim(wiki, seed["id"])["status"],
                         seed["status"])
        self.assertEqual(review.show(wiki, i1)["status"], "open")
        self.assertEqual(review.show(wiki, i2)["status"], "open")

    def test_receipt_revision_is_int_after(self):
        """S2: apply_batch's revision is the post-commit int, not the
        {before, after} receipt dict."""
        wiki = fresh()
        seed_claim(wiki, "order.service", "ttl", 300)
        i1 = tier_item(wiki, "order_service", "t1")
        rev = wiki.revision()
        out = review.apply_batch(wiki, [
            {"id": i1, "action": "defer", "params": {}},
        ], rev)
        self.assertIsInstance(out["revision"], int)
        self.assertEqual(out["revision"], rev + 1)


if __name__ == "__main__":
    unittest.main()
