import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, deps, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import ConflictError, Transaction


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "compile-test")
    return wiki


def ingest_text(wiki, ref, body):
    return sources.ingest(wiki, "text", ref, body.encode("utf-8"))


def candidate(wiki, receipt, subject="project.analytics.attribution",
              predicate="click_lookback_window", value=7,
              authority="explicit_project_decision", scope=None, valid_from=None):
    return {
        "id": "auto",
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "scope": scope or {"environment": "production"},
        "valid_from": valid_from,
        "valid_to": None,
        "recorded_at": "2026-09-15T00:00:00Z",
        "status": "candidate",
        "proposed_by": "agent",
        "authority": {"type": authority, "source": "ADR"},
        "evidence": [{
            "source_id": receipt["source_id"],
            "source_version": receipt["version"],
            "locator": {"type": "heading", "value": "Window"},
        }],
        "supersedes": [],
    }


def run_compile(wiki, receipt, candidates, classifications, extra_report=None):
    report = extra_report or {
        "source_id": receipt["source_id"],
        "source_version": receipt["version"],
        "coverage": {"text": "complete", "tables": "skipped", "images": "skipped",
                     "diagrams": "skipped", "formulas": "skipped"},
        "warnings": [],
        "parser": {"name": "wikicore-raw", "version": "1.0"},
    }
    run_id = wc_compile.stage_candidates(wiki, receipt["source_id"], receipt["version"],
                                         candidates, report)
    prep = wc_compile.reconcile_prepare(wiki, run_id)
    receipt_run = wc_compile.reconcile_apply(wiki, run_id, classifications, wiki.revision())
    return run_id, prep, receipt_run


class TestCompile(unittest.TestCase):
    def test_unrelated_new_claim_auto_accepted(self):
        wiki = fresh()
        r = ingest_text(wiki, "adr", "# ADR\nseven days")
        c = candidate(wiki, r)
        _, prep, rr = run_compile(wiki, r, [c],
                                  [{"index": 0, "relationship": "UNRELATED",
                                    "target_claim_id": None, "valid_from": None,
                                    "valid_to": None,
                                    "authority": {"type": "explicit_project_decision"}}])
        self.assertEqual(rr["changes"]["claims_created"], 1)
        stored = claims.list_claims(wiki)[0]
        self.assertEqual(stored["status"], "accepted")
        self.assertEqual(stored["root_origin"], r["source_id"])
        self.assertEqual(sources.pending_versions(wiki), [])

    def test_correction_supersedes_and_invalidates(self):
        wiki = fresh()
        r1 = ingest_text(wiki, "adr-old", "window is 30 days")
        old_c = candidate(wiki, r1, value=30)
        run_compile(wiki, r1, [old_c],
                    [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
                      "valid_from": None, "valid_to": None,
                      "authority": {"type": "explicit_project_decision"}}])
        old = claims.list_claims(wiki)[0]
        # a derived page depends on the old claim
        txn = Transaction(wiki, "page")
        deps.register(txn, "wiki/concepts/attribution.md",
                      ["%s@%d" % (old["id"], old["version"])])
        txn.commit(wiki.revision())

        r2 = ingest_text(wiki, "memo", "changed to 7 days")
        new_c = candidate(wiki, r2, value=7)
        _, prep, rr = run_compile(wiki, r2, [new_c],
                                  [{"index": 0, "relationship": "CORRECTION",
                                    "target_claim_id": old["id"], "valid_from": None,
                                    "valid_to": None,
                                    "authority": {"type": "explicit_project_decision"}}])
        old_after = claims.load_claim(wiki, old["id"])
        new_claim = [c for c in claims.list_claims(wiki) if c["id"] != old["id"]][0]
        self.assertEqual(old_after["status"], "superseded")
        self.assertEqual(new_claim["status"], "accepted")
        self.assertIn(old["id"], new_claim["supersedes"])
        self.assertTrue(deps.is_stale(wiki, "wiki/concepts/attribution.md"))
        self.assertEqual(rr["changes"]["claims_superseded"], 1)

    def test_policy_change_sets_valid_to(self):
        wiki = fresh()
        r1 = ingest_text(wiki, "old", "30 days before")
        old_c = candidate(wiki, r1, value=30, valid_from="2026-01-01")
        run_compile(wiki, r1, [old_c],
                    [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
                      "valid_from": "2026-01-01", "valid_to": None,
                      "authority": {"type": "explicit_project_decision"}}])
        old = claims.list_claims(wiki)[0]
        r2 = ingest_text(wiki, "new", "7 days from Sept")
        new_c = candidate(wiki, r2, value=7, valid_from="2026-09-01")
        run_compile(wiki, r2, [new_c],
                    [{"index": 0, "relationship": "POLICY_CHANGE",
                      "target_claim_id": old["id"], "valid_from": "2026-09-01",
                      "valid_to": None,
                      "authority": {"type": "explicit_project_decision"}}])
        self.assertEqual(claims.load_claim(wiki, old["id"])["valid_to"], "2026-08-31")

    def test_scope_difference_creates_second_claim(self):
        wiki = fresh()
        r1 = ingest_text(wiki, "prod", "prod window")
        prod_c = candidate(wiki, r1, scope={"environment": "production"})
        run_compile(wiki, r1, [prod_c],
                    [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
                      "valid_from": None, "valid_to": None,
                      "authority": {"type": "explicit_project_decision"}}])
        prod = claims.list_claims(wiki)[0]
        r2 = ingest_text(wiki, "sandbox", "sandbox window")
        sb_c = candidate(wiki, r2, value=30, scope={"environment": "sandbox"})
        run_compile(wiki, r2, [sb_c],
                    [{"index": 0, "relationship": "SCOPE_DIFFERENCE",
                      "target_claim_id": prod["id"], "valid_from": None,
                      "valid_to": None,
                      "authority": {"type": "explicit_project_decision"}}])
        all_claims = claims.list_claims(wiki)
        self.assertEqual(len(all_claims), 2)
        self.assertEqual(claims.load_claim(wiki, prod["id"])["status"], "accepted")

    def test_contradiction_goes_to_review(self):
        wiki = fresh()
        r1 = ingest_text(wiki, "a", "claim a")
        c1 = candidate(wiki, r1, authority="agent_inference")
        run_compile(wiki, r1, [c1],
                    [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
                      "valid_from": None, "valid_to": None,
                      "authority": {"type": "agent_inference"}}])
        target = claims.list_claims(wiki)[0]
        r2 = ingest_text(wiki, "b", "conflicting b")
        c2 = candidate(wiki, r2, value=999, authority="agent_inference")
        _, prep, rr = run_compile(wiki, r2, [c2],
                                  [{"index": 0, "relationship": "CONTRADICTION",
                                    "target_claim_id": target["id"], "valid_from": None,
                                    "valid_to": None,
                                    "authority": {"type": "agent_inference"}}])
        self.assertEqual(len(rr["conflicts"]), 1)
        disputed = [c for c in claims.list_claims(wiki) if c["status"] == "disputed"]
        self.assertEqual(len(disputed), 1)
        items = wiki.read_jsonl(".state/review-queue.jsonl")
        self.assertEqual(items[0]["kind"], "possible_contradiction")
        self.assertEqual(items[0]["status"], "open")

    def test_duplicate_merges_evidence(self):
        wiki = fresh()
        r1 = ingest_text(wiki, "orig", "original statement")
        c1 = candidate(wiki, r1)
        run_compile(wiki, r1, [c1],
                    [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
                      "valid_from": None, "valid_to": None,
                      "authority": {"type": "explicit_project_decision"}}])
        target = claims.list_claims(wiki)[0]
        r2 = ingest_text(wiki, "dup", "same statement repeated")
        c2 = candidate(wiki, r2)
        _, prep, rr = run_compile(wiki, r2, [c2],
                                  [{"index": 0, "relationship": "DUPLICATE",
                                    "target_claim_id": target["id"], "valid_from": None,
                                    "valid_to": None,
                                    "authority": {"type": "explicit_project_decision"}}])
        self.assertEqual(rr["changes"].get("claims_created", 0), 0)
        self.assertEqual(rr["changes"]["claims_updated"], 1)
        merged = claims.load_claim(wiki, target["id"])
        self.assertEqual(len(merged["evidence"]), 2)

    def test_corroboration_auto_accepts_at_threshold(self):
        wiki = fresh()
        r1 = ingest_text(wiki, "one", "first origin")
        c1 = candidate(wiki, r1, authority="agent_inference")
        run_compile(wiki, r1, [c1],
                    [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
                      "valid_from": None, "valid_to": None,
                      "authority": {"type": "agent_inference"}}])
        target = claims.list_claims(wiki)[0]
        self.assertEqual(target["status"], "candidate")
        r2 = ingest_text(wiki, "two", "second independent origin")
        c2 = candidate(wiki, r2)
        run_compile(wiki, r2, [c2],
                    [{"index": 0, "relationship": "CORROBORATION",
                      "target_claim_id": target["id"], "valid_from": None,
                      "valid_to": None,
                      "authority": {"type": "independent_corroboration"}}])
        got = claims.load_claim(wiki, target["id"])
        self.assertEqual(got["status"], "accepted")
        self.assertEqual(got["acceptance"]["reason"], "independent_corroboration")

    def test_stale_base_revision_is_atomic(self):
        wiki = fresh()
        r = ingest_text(wiki, "x", "body")
        c = candidate(wiki, r)
        run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"], [c], {
            "source_id": r["source_id"], "source_version": r["version"],
            "coverage": {"text": "complete"}, "warnings": [],
            "parser": {"name": "x"},
        })
        before = len(claims.list_claims(wiki))
        with self.assertRaises(ConflictError):
            wc_compile.reconcile_apply(wiki, run_id,
                                       [{"index": 0, "relationship": "UNRELATED",
                                         "target_claim_id": None, "valid_from": None,
                                         "valid_to": None,
                                         "authority": {"type": "explicit_project_decision"}}],
                                       wiki.revision() + 5)
        self.assertEqual(len(claims.list_claims(wiki)), before)

    def test_prepare_matches_by_subject_predicate(self):
        wiki = fresh()
        r1 = ingest_text(wiki, "one", "body one")
        c1 = candidate(wiki, r1)
        run_compile(wiki, r1, [c1],
                    [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
                      "valid_from": None, "valid_to": None,
                      "authority": {"type": "explicit_project_decision"}}])
        existing = claims.list_claims(wiki)[0]
        r2 = ingest_text(wiki, "two", "body two")
        c2 = candidate(wiki, r2)
        run_id = wc_compile.stage_candidates(wiki, r2["source_id"], r2["version"], [c2], {
            "source_id": r2["source_id"], "source_version": 1,
            "coverage": {"text": "complete"}, "warnings": [], "parser": {"name": "x"},
        })
        prep = wc_compile.reconcile_prepare(wiki, run_id)
        self.assertEqual(len(prep["comparisons"][0]["matches"]), 1)
        self.assertEqual(prep["comparisons"][0]["matches"][0]["claim_id"], existing["id"])
        self.assertTrue(prep["comparisons"][0]["matches"][0]["same_scope"])


if __name__ == "__main__":
    unittest.main()
