import json, os, subprocess, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, deps, sources
from wikicore.transaction import ConflictError, Transaction, TxnError
from wikicore.store import Wiki, init_wiki


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


    def test_auto_verdict_unrelated(self):
        self.assertEqual(wc_compile.auto_verdict({"root_origin": "o1"}, []),
                         "UNRELATED")

    def test_auto_verdict_duplicate_same_origin(self):
        m = [{"same_scope": True, "same_value": True, "status": "accepted",
              "root_origins": ["o1"]}]
        self.assertEqual(wc_compile.auto_verdict({"root_origin": "o1"}, m),
                         "DUPLICATE")

    def test_auto_verdict_corroboration_diff_origin(self):
        m = [{"same_scope": True, "same_value": True, "status": "accepted",
              "root_origins": ["o2"]}]
        self.assertEqual(wc_compile.auto_verdict({"root_origin": "o1"}, m),
                         "CORROBORATION")

    def test_auto_verdict_needs_review_on_value_diff(self):
        m = [{"same_scope": True, "same_value": False, "status": "accepted",
              "root_origins": ["o2"]}]
        self.assertIsNone(wc_compile.auto_verdict({"root_origin": "o1"}, m))

    def test_auto_verdict_needs_review_multi_match(self):
        m = [{"same_scope": True, "same_value": True, "status": "accepted",
              "root_origins": ["o2"]},
             {"same_scope": False, "same_value": False, "status": "accepted",
              "root_origins": ["o3"]}]
        self.assertIsNone(wc_compile.auto_verdict({"root_origin": "o1"}, m))

    def test_apply_autofills_unrelated(self):
        wiki = fresh()
        rcpt = ingest_text(wiki, "doc", "x")
        c = candidate(wiki, rcpt)
        run = wc_compile.stage_candidates(wiki, rcpt["source_id"],
                                          rcpt["version"], [c], {})
        wc_compile.reconcile_prepare(wiki, run)
        receipt = wc_compile.reconcile_apply(wiki, run, [], wiki.revision())
        self.assertEqual(receipt["changes"]["claims_created"], 1)
        self.assertEqual(receipt["auto"]["unrelated"], 1)

    def test_apply_rejects_undecidable_gap(self):
        wiki = fresh()
        rcpt = ingest_text(wiki, "doc", "x")
        c = candidate(wiki, rcpt)
        run1 = wc_compile.stage_candidates(wiki, rcpt["source_id"],
                                           rcpt["version"], [c], {})
        wc_compile.reconcile_prepare(wiki, run1)
        wc_compile.reconcile_apply(wiki, run1,
                                   [{"index": 0, "relationship": "UNRELATED"}],
                                   wiki.revision())
        c2 = candidate(wiki, rcpt, value=8)
        rcpt2 = ingest_text(wiki, "doc2", "y")
        c2["evidence"] = [{"source_id": rcpt2["source_id"],
                           "source_version": rcpt2["version"],
                           "locator": {"type": "heading", "value": "Window"}}]
        run2 = wc_compile.stage_candidates(wiki, rcpt2["source_id"],
                                           rcpt2["version"], [c2], {})
        wc_compile.reconcile_prepare(wiki, run2)
        with self.assertRaises(TxnError):
            wc_compile.reconcile_apply(wiki, run2, [], wiki.revision())

    def test_reconcile_prepare_compact(self):
        wiki = fresh()
        rcpt = ingest_text(wiki, "doc", "x")
        c = candidate(wiki, rcpt)
        run = wc_compile.stage_candidates(wiki, rcpt["source_id"],
                                          rcpt["version"], [c], {})
        out = wc_compile.reconcile_prepare(wiki, run)
        row = out["comparisons"][0]
        self.assertEqual(row["auto"], "UNRELATED")
        self.assertEqual(row["key"], "project.analytics.attribution/click_lookback_window")
        self.assertNotIn("candidate", row)   # no full echo
        self.assertIn("candidate_value", row)

class TestUmbrella(unittest.TestCase):
    def test_umbrella_full_flow(self):
        wiki = fresh()
        ingest_text(wiki, "doc", "fact: window is 7 days")

        out1 = wc_compile.compile_umbrella(wiki, resume=False)
        self.assertEqual(len(out1["pending"]), 1)
        p = out1["pending"][0]
        self.assertTrue(os.path.isfile(p["content_path"]))
        with open(p["candidates_path"], "w") as f:
            f.write(json.dumps({
                "subject": "project.window", "predicate": "days", "value": 7,
                "locator": "h:Window", "authority": "manual"}) + "\n")

        out2 = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out2["auto"]["unrelated"], 1)
        self.assertEqual(out2["needs_review"], [])
        self.assertFalse(out2["needs_classifications"])

        out3 = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertTrue(out3["verify"]["ok"])
        self.assertEqual(out3["changes"]["claims_created"], 1)

    def test_umbrella_resume_waits_for_classifications(self):
        wiki = fresh()
        ingest_text(wiki, "a", "x")
        out = wc_compile.compile_umbrella(wiki, resume=False)
        p = out["pending"][0]
        with open(p["candidates_path"], "w") as f:
            f.write(json.dumps({"subject": "s", "predicate": "p",
                                "value": 1, "locator": "h:H"}) + "\n")
        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertFalse(out["needs_classifications"])

    def test_umbrella_needs_review_round_trip(self):
        wiki = fresh()
        rcpt1 = ingest_text(wiki, "s1", "first source")
        run_compile(wiki, rcpt1, [candidate(wiki, rcpt1)], [])

        ingest_text(wiki, "s2", "second source")
        out1 = wc_compile.compile_umbrella(wiki, resume=False)
        p = out1["pending"][0]
        with open(p["candidates_path"], "w") as f:
            f.write(json.dumps({
                "subject": "project.analytics.attribution",
                "predicate": "click_lookback_window", "value": 30,
                "locator": "h:Other", "authority": "doc"}) + "\n")
        out2 = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertTrue(out2["needs_classifications"])
        row = out2["needs_review"][0]
        self.assertEqual(row["source_id"], p["source_id"])
        target = row["matches"][0]["claim_id"]
        with open(p["classifications_path"], "w") as f:
            json.dump([{"index": 0, "relationship": "CORRECTION",
                        "target_claim_id": target}], f)
        out3 = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertFalse(out3["needs_classifications"])
        self.assertEqual(out3["changes"]["claims_created"], 1)
        self.assertEqual(out3["changes"]["claims_superseded"], 1)
        self.assertTrue(out3["verify"]["ok"], out3.get("verify"))
        out4 = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out4["phase"], "done")
        self.assertEqual(out4["changes"]["claims_created"], 1)
        self.assertEqual(out4["changes"]["claims_superseded"], 1)
        self.assertTrue(out4["verify"]["ok"], out4.get("verify"))

    def test_stage_candidates_accepts_jsonl(self):
        wiki = fresh()
        rcpt = ingest_text(wiki, "doc", "fact: window is 7 days")
        path = os.path.join(wiki.root, "cands.jsonl")
        with open(path, "w") as f:
            f.write(json.dumps({"subject": "s", "predicate": "p", "value": 1,
                                "locator": "h:H"}) + "\n")
            f.write(json.dumps({"subject": "s2", "predicate": "p2", "value": 2,
                                "locator": "h:H2"}) + "\n")
        script = os.path.join(os.path.dirname(__file__), "..", "wiki.py")
        out = subprocess.run(
            [sys.executable, script, "--root", wiki.root,
             "stage-candidates", "--file", path,
             "--source-id", rcpt["source_id"], "--source-version", "1"],
            capture_output=True, text=True,
            cwd=os.path.join(os.path.dirname(__file__), ".."))
        self.assertEqual(out.returncode, 0, out.stderr)
        payload = json.loads(out.stdout)
        self.assertEqual(payload["staged"], 2)
        rundir = wiki.p(".state", "staging", payload["run_id"])
        with open(os.path.join(rundir, "compile.json")) as f:
            staged = json.load(f)
        self.assertEqual(len(staged["candidates"]), 2)
        self.assertTrue(all(c.get("id", "").startswith("claim_")
                            for c in staged["candidates"]))
        self.assertEqual(staged["candidates"][0]["authority"]["type"],
                         "authoritative_source")

    def test_compile_cli_end_to_end(self):
        wiki = fresh()
        ingest_text(wiki, "doc", "fact: window is 7 days")
        script = os.path.join(os.path.dirname(__file__), "..", "wiki.py")
        cwd = os.path.join(os.path.dirname(__file__), "..")
        r = subprocess.run([sys.executable, script, "--root", wiki.root, "compile"],
                           capture_output=True, text=True, cwd=cwd)
        self.assertEqual(r.returncode, 0, r.stderr)
        pending = json.loads(r.stdout)["pending"]
        self.assertEqual(len(pending), 1)
        with open(pending[0]["candidates_path"], "w") as f:
            f.write(json.dumps({"subject": "project.window", "predicate": "days",
                                "value": 7, "locator": "h:Window"}) + "\n")
        r = subprocess.run([sys.executable, script, "--root", wiki.root,
                            "compile", "--resume"],
                           capture_output=True, text=True, cwd=cwd)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(json.loads(r.stdout)["verify"]["ok"])

if __name__ == "__main__":
    unittest.main()
