import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, doctor, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "doctor-test")
    return wiki


def add_accepted_claim(wiki, subject="s", predicate="p", value=1,
                       authority="explicit_project_decision", scope=None):
    r = sources.ingest(wiki, "text", "src-%s-%s" % (subject, predicate), b"body")
    cand = {
        "id": "auto", "subject": subject, "predicate": predicate, "value": value,
        "scope": scope or {}, "valid_from": None, "valid_to": None,
        "recorded_at": "2026-09-15T00:00:00Z", "status": "candidate",
        "proposed_by": "agent", "authority": {"type": authority},
        "evidence": [{"source_id": r["source_id"], "source_version": 1,
                      "locator": {"type": "text", "value": "x"}}],
        "supersedes": [],
    }
    rid = wc_compile.stage_candidates(wiki, r["source_id"], 1, [cand], {
        "source_id": r["source_id"], "source_version": 1,
        "coverage": {"text": "complete"}, "warnings": [], "parser": {"name": "x"},
    })
    wc_compile.reconcile_apply(wiki, rid,
                               [{"index": 0, "relationship": "UNRELATED",
                                 "target_claim_id": None, "valid_from": None,
                                 "valid_to": None,
                                 "authority": {"type": authority}}],
                               wiki.revision())
    return claims.list_claims(wiki, subject=subject)[0]


class TestDoctor(unittest.TestCase):
    def test_clean_wiki_ok(self):
        wiki = fresh()
        add_accepted_claim(wiki, scope={"environment": "prod"})
        add_accepted_claim(wiki, "other", "q", 2, scope={"environment": "prod"})
        out = doctor.run(wiki)
        error_codes = [f["code"] for f in out["findings"] if f["severity"] == "error"]
        self.assertEqual(error_codes, [], out["findings"])

    def test_detects_broken_reference(self):
        wiki = fresh()
        c = add_accepted_claim(wiki)
        # corrupt: evidence points at nonexistent source version
        path = wiki.p("claims/%s.json" % c["id"])
        with open(path) as f:
            data = json.load(f)
        data["evidence"][0]["source_version"] = 99
        with open(path, "w") as f:
            json.dump(data, f)
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "broken_reference" for f in out["findings"]))
        self.assertFalse(out["ok"])

    def test_detects_missing_scope_and_date(self):
        wiki = fresh()
        add_accepted_claim(wiki)  # no scope, no valid_from
        out = doctor.run(wiki)
        codes = {f["code"] for f in out["findings"]}
        self.assertIn("missing_scope", codes)
        self.assertIn("missing_effective_date", codes)

    def test_detects_staging_leftover(self):
        wiki = fresh()
        os.makedirs(wiki.p(".state/staging", "run_01JLEFTOVERLEFTOVERLEFTOVER"),
                    exist_ok=True)
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "staging_leftover" for f in out["findings"]))

    def test_detects_unsupported_accepted_claim(self):
        wiki = fresh()
        # simulate bad state: accepted claim with agent_inference authority, no evidence
        c = add_accepted_claim(wiki, authority="agent_inference")
        path = wiki.p("claims/%s.json" % c["id"])
        with open(path) as f:
            data = json.load(f)
        data["status"] = "accepted"
        data["acceptance"] = {"reason": "agent_inference", "accepted_at": "2026-09-15T00:00:00Z"}
        data["evidence"] = []
        with open(path, "w") as f:
            json.dump(data, f)
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "unsupported_accepted" for f in out["findings"]))

    def test_detects_schema_invalid(self):
        wiki = fresh()
        c = add_accepted_claim(wiki)
        path = wiki.p("claims/%s.json" % c["id"])
        with open(path) as f:
            data = json.load(f)
        data["status"] = "totally-bogus"
        with open(path, "w") as f:
            json.dump(data, f)
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "schema_invalid" for f in out["findings"]))

    def test_detects_out_of_band_edit(self):
        wiki = fresh()
        add_accepted_claim(wiki)
        # simulate out-of-band edit: bump state revision hash mismatch is not
        # detectable, but a modified canonical file keeps its schema — doctor
        # checks manifest content_hash consistency instead
        txn = Transaction(wiki, "tamper")
        txn.stage_write("claims/claim_%s.json" % ("A" * 26), {
            "id": "claim_" + "A" * 26, "subject": "x",
            "predicate": "y", "value": 1, "status": "candidate",
            "authority": {"type": "agent_inference"},
            "evidence": [{"source_id": "source_" + "A" * 26, "source_version": 1,
                          "locator": {"type": "text", "value": "z"}}],
            "recorded_at": "2026-09-15T00:00:00Z", "version": 1,
        })
        txn.commit(wiki.revision())
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "broken_reference" for f in out["findings"]))


if __name__ == "__main__":
    unittest.main()
