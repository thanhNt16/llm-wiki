import json, math, os, sys, tempfile, time, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, contextpack, deps, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError

def fresh(budget_default=6000):
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "ctx-test")
    cfg = wiki.load_config()
    cfg["context_budget_default"] = budget_default
    wiki.save_config(cfg)
    return wiki

def add_claim(wiki, subject, predicate, value=1, authority="explicit_project_decision",
              scope=None, ref=None):
    r = sources.ingest(wiki, "text", ref or ("src-" + subject + predicate),
                       ("body of %s %s" % (subject, predicate)).encode())
    cand = {
        "id": "auto", "subject": subject, "predicate": predicate, "value": value,
        "scope": scope or {}, "valid_from": None, "valid_to": None,
        "recorded_at": "2026-09-15T00:00:00Z", "status": "candidate",
        "proposed_by": "agent", "authority": {"type": authority, "source": "t"},
        "evidence": [{"source_id": r["source_id"], "source_version": r["version"],
                      "locator": {"type": "text", "value": "x"}}],
        "supersedes": [],
    }
    run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"], [cand], {
        "source_id": r["source_id"], "source_version": r["version"],
        "coverage": {"text": "complete"}, "warnings": [], "parser": {"name": "x"},
    })
    wc_compile.reconcile_apply(wiki, run_id,
                               [{"index": 0, "relationship": "UNRELATED",
                                 "target_claim_id": None, "valid_from": None,
                                 "valid_to": None,
                                 "authority": {"type": authority}}],
                               wiki.revision())
    return claims.list_claims(wiki, subject=subject)[0]

def add_decision(wiki, title, body=""):
    d = {
        "id": "decision_" + claims.ids.new("decision").split("_", 1)[1],
        "title": title, "status": "accepted", "decided_on": "2026-01-10",
        "recorded_at": "2026-09-15T00:00:00Z", "version": 1,
        "claims": [], "evidence": [], "body": body,
    }
    txn = Transaction(wiki, "t")
    claims.save_decision(txn, d)
    txn.commit(wiki.revision())
    return d

class TestContextPack(unittest.TestCase):
    def test_pack_respects_hard_budget(self):
        wiki = fresh()
        for i in range(20):
            add_claim(wiki, "topic%d" % i, "detail", value=i)
        out = contextpack.build(wiki, task="topic1 detail", budget=300)
        receipt = out["receipt"]
        self.assertLessEqual(receipt["budget"]["estimated"], 300)
        text_len = os.path.getsize(wiki.p(out["pack_path"]))
        self.assertLessEqual(math.ceil(text_len / 4), 300 + 1)  # +1 rounding slack

    def test_priority_constraints_first(self):
        wiki = fresh()
        constraint = add_claim(wiki, "security", "must_encrypt_pii",
                               authority="explicit_project_decision")
        generic = add_claim(wiki, "notes", "color", value="blue",
                            authority="agent_inference")
        # narrow task (Q5): matching constraint included, off-topic claim omitted
        out = contextpack.build(wiki, task="must encrypt pii", budget=800)
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("must_encrypt_pii", pack)
        self.assertNotIn("color", pack)

    def test_offtopic_sections_omitted_with_markers(self):
        wiki = fresh()
        add_claim(wiki, "payments", "retention_days", value=365)
        add_decision(wiki, "Use PostgreSQL", body="We chose Postgres.")
        out = contextpack.build(wiki, task="payment retention", budget=4000)
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("## Omitted sections", pack)
        self.assertIn("Accepted decisions (off-topic)", pack)
        self.assertTrue(any("Accepted decisions (off-topic)" in o
                            for o in out["receipt"]["omitted"]))

    def test_concept_pages_scored_by_content(self):
        wiki = fresh()
        add_claim(wiki, "payments", "retention_days", value=365)
        os.makedirs(wiki.p("wiki/concepts"), exist_ok=True)
        with open(wiki.p("wiki/concepts/alpha.md"), "w") as f:
            f.write("---\ntype: concept\nsubject: payments\ntitle: Notes A\n"
                    "created: 2026-09-15T00:00:00Z\ndeps: []\nstale: false\n---\n\n"
                    "Retry backoff uses exponential delays.\n")
        with open(wiki.p("wiki/concepts/beta.md"), "w") as f:
            f.write("---\ntype: concept\nsubject: caching\ntitle: Notes B\n"
                    "created: 2026-09-15T00:00:00Z\ndeps: []\nstale: false\n---\n\n"
                    "Cache invalidation gossip.\n")
        out = contextpack.build(wiki, task="retry backoff", budget=4000)
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("wiki/concepts/alpha.md", pack)
        self.assertNotIn("beta.md", pack)

    def test_receipt_lists_exact_dependency_versions(self):
        wiki = fresh()
        c = add_claim(wiki, "db", "engine", value="postgres")
        out = contextpack.build(wiki, task="db engine postgres", budget=4000)
        self.assertIn("%s@%d" % (c["id"], c["version"]), out["receipt"]["claims"])
        self.assertTrue(out["receipt"]["context_id"].startswith("context_"))

    def test_pack_registered_and_invalidated(self):
        wiki = fresh()
        c = add_claim(wiki, "db", "engine", value="postgres")
        out = contextpack.build(wiki, task="db engine", budget=4000)
        stale = deps.invalidate(wiki, [c["id"]])
        self.assertIn(out["pack_path"], stale)

    def test_resume_lists_changes_since_last_pack(self):
        wiki = fresh()
        c1 = add_claim(wiki, "window", "days", value=30)
        first = contextpack.build(wiki, task="window", budget=4000)
        # supersede c1 via correction
        r = sources.ingest(wiki, "text", "memo", b"now 7")
        cand = {
            "id": "auto", "subject": "window", "predicate": "days", "value": 7,
            "scope": {}, "valid_from": None, "valid_to": None,
            "recorded_at": "2026-09-15T01:00:00Z", "status": "candidate",
            "proposed_by": "agent", "authority": {"type": "explicit_project_decision"},
            "evidence": [{"source_id": r["source_id"], "source_version": 1,
                          "locator": {"type": "text", "value": "x"}}],
            "supersedes": [],
        }
        run_id = wc_compile.stage_candidates(wiki, r["source_id"], 1, [cand], {
            "source_id": r["source_id"], "source_version": 1,
            "coverage": {"text": "complete"}, "warnings": [], "parser": {"name": "x"},
        })
        wc_compile.reconcile_apply(wiki, run_id,
                                   [{"index": 0, "relationship": "CORRECTION",
                                     "target_claim_id": c1["id"], "valid_from": None,
                                     "valid_to": None,
                                     "authority": {"type": "explicit_project_decision"}}],
                                   wiki.revision())
        out = contextpack.build(wiki, task="", budget=4000, resume=True)
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn(c1["id"], pack)
        self.assertIn("superseded", pack.lower())

    def test_changes_since_specific_context(self):
        wiki = fresh()
        add_claim(wiki, "a", "x")
        first = contextpack.build(wiki, task="a x", budget=4000)
        add_claim(wiki, "b", "y")
        out = contextpack.build(wiki, task="", budget=4000,
                                changes_since=first["context_id"])
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("b / y", pack)  # claim b/y appears as changed

class TestBriefingReference(unittest.TestCase):
    def test_changes_since_matches_requested_receipt(self):
        # regression: the OLDEST eligible receipt used to win; the pack whose
        # id was passed must be the diff base
        wiki = fresh()
        add_claim(wiki, "a", "x")
        first = contextpack.build(wiki, task="a x", budget=4000)
        add_claim(wiki, "b", "y")
        second = contextpack.build(wiki, task="b y", budget=4000)
        out = contextpack.build(wiki, task="", budget=4000,
                                changes_since=second["context_id"])
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        # the requested receipt is the diff base, not the oldest eligible one
        self.assertIn(second["context_id"], pack)
        self.assertNotIn(first["context_id"], pack)

    def test_changes_since_accepts_timestamp(self):
        wiki = fresh()
        add_claim(wiki, "db", "engine", value="postgres")
        out = contextpack.build(wiki, task="", budget=4000,
                                changes_since="2000-01-01T00:00:00Z")
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertNotIn("No committed wiki operations", pack)
        self.assertIn("db / engine", pack)

    def test_changes_since_future_timestamp_reports_no_ops(self):
        # previously a timestamp fell through to project-start and diffed
        # everything; now it is the actual reference
        wiki = fresh()
        add_claim(wiki, "db", "engine", value="postgres")
        out = contextpack.build(wiki, task="", budget=4000,
                                changes_since="2999-01-01")
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("2999-01-01", pack)
        self.assertIn("No committed wiki operations", pack)

    def test_superseded_resolution_uses_superseded_by(self):
        wiki = fresh()
        add_claim(wiki, "seed", "s")  # one committed op so the briefing runs
        old_id, new_id = "claim_" + "A" * 26, "claim_" + "B" * 26
        base = {"scope": {}, "valid_from": None, "valid_to": None,
                "recorded_at": "2026-09-15T00:00:00Z", "proposed_by": "agent",
                "authority": {"type": "explicit_project_decision"},
                "supersedes": [], "version": 1,
                "evidence": [{"source_id": "source_" + "A" * 26,
                              "source_version": 1,
                              "locator": {"type": "text", "value": "x"}}]}
        old = dict(base, id=old_id, subject="cache", predicate="ttl",
                   value=60, status="superseded", superseded_by=new_id,
                   updated_at=time.strftime("%Y-%m-%dT%H:%M:%SZ",
                                            time.gmtime()))
        new = dict(base, id=new_id, subject="cache", predicate="ttl", value=30,
                   status="accepted", supersedes=[])
        for c in (old, new):
            with open(wiki.p("claims/%s.json" % c["id"]), "w") as f:
                json.dump(c, f)
        out = contextpack.build(wiki, task="", budget=4000,
                                changes_since="2000-01-01")
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("was 60 → now 30", pack)

    def test_superseded_resolution_guards_self_reference(self):
        wiki = fresh()
        add_claim(wiki, "seed", "s")
        c = add_claim(wiki, "loop", "self")
        path = wiki.p("claims/%s.json" % c["id"])
        with open(path) as f:
            data = json.load(f)
        data["status"] = "superseded"
        data["superseded_by"] = data["id"]  # self-reference must not resolve
        with open(path, "w") as f:
            json.dump(data, f)
        out = contextpack.build(wiki, task="", budget=4000,
                                changes_since="2000-01-01")
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("now ?", pack)

class TestBudgetAndTruncation(unittest.TestCase):
    def test_budget_zero_is_error(self):
        wiki = fresh()
        with self.assertRaises(TxnError):
            contextpack.build(wiki, task="", budget=0)

    def test_tiny_budget_error_names_minimum(self):
        wiki = fresh()
        add_claim(wiki, "db", "engine", value="postgres")
        try:
            contextpack.build(wiki, task="", budget=10)
            self.fail("expected TxnError")
        except TxnError as e:
            self.assertIn("minimum required", str(e))

    def test_decision_deps_pin_real_versions(self):
        wiki = fresh()
        d = add_decision(wiki, "Use PostgreSQL", body="We chose Postgres.")
        txn = Transaction(wiki, "t2")
        claims.save_decision(txn, d)  # bump to version 2
        txn.commit(wiki.revision())
        out = contextpack.build(wiki, task="", budget=4000)
        with open(wiki.p(out["pack_path"])) as f:
            pack = f.read()
        self.assertIn("%s@2" % d["id"], pack)

    def test_deps_truncation_recorded_edges_complete(self):
        wiki = fresh()
        for i in range(6):
            add_claim(wiki, "claim%02d" % i, "p", value=i)
        old_limit = contextpack._DEPS_LIMIT
        contextpack._DEPS_LIMIT = 3
        try:
            out = contextpack.build(wiki, task="", budget=4000)
        finally:
            contextpack._DEPS_LIMIT = old_limit
        receipt = out["receipt"]
        self.assertEqual(receipt["deps_truncated"], 3)
        # registration must keep the FULL dep set so any later claim edit
        # still marks this pack stale
        edges = deps.graph(wiki)["edges"][out["pack_path"]]
        self.assertEqual(len(edges), 6)
        all_claim_ids = {c["id"] for c in claims.list_claims(wiki)}
        self.assertTrue(all(dep.split("@")[0] in all_claim_ids
                            for dep in edges))

if __name__ == "__main__":
    unittest.main()
