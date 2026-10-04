import json, math, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, contextpack, deps, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction

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

if __name__ == "__main__":
    unittest.main()
