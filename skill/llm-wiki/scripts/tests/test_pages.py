import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, deps, pages, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "pages-test")
    return wiki


class TestPages(unittest.TestCase):
    def _wiki_with_claim_and_decision(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "adr", b"decision text")
        cand = {
            "id": "auto", "subject": "db", "predicate": "engine", "value": "postgres",
            "scope": {}, "valid_from": None, "valid_to": None,
            "recorded_at": "2026-09-15T00:00:00Z", "status": "candidate",
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
                                   [{"index": 0, "relationship": "UNRELATED",
                                     "target_claim_id": None, "valid_from": None,
                                     "valid_to": None,
                                     "authority": {"type": "explicit_project_decision"}}],
                                   wiki.revision())
        claim = claims.list_claims(wiki)[0]
        d = {
            "id": claims.ids.new("decision"),
            "title": "Use Postgres", "status": "accepted", "decided_on": "2026-01-10",
            "recorded_at": "2026-09-15T00:00:00Z", "version": 1,
            "claims": [claim["id"]],
            "evidence": [{"source_id": r["source_id"], "source_version": 1,
                          "locator": {"type": "text", "value": "x"}}],
            "body": "chose postgres",
        }
        txn = Transaction(wiki, "d")
        claims.save_decision(txn, d)
        txn.commit(wiki.revision())
        return wiki, claim, d

    def test_build_pages_creates_index_and_decision_page(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        rr = pages.build_pages(wiki, wiki.revision())
        idx_path = wiki.p("wiki/index.md")
        self.assertTrue(os.path.isfile(idx_path))
        page = wiki.p("wiki/decisions/%s.md" % d["id"])
        self.assertTrue(os.path.isfile(page))
        with open(page) as f:
            body = f.read()
        self.assertIn("Use Postgres", body)
        self.assertIn(claim["id"], body)
        self.assertFalse(deps.stale_artifacts(wiki))
        v = pages.verify(wiki)
        self.assertTrue(v["ok"], v["errors"])

    def test_verify_reports_stale_after_invalidate(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        deps.invalidate(wiki, [claim["id"]])
        v = pages.verify(wiki)
        self.assertFalse(v["ok"])
        self.assertTrue(any("stale" in e for e in v["errors"]))
        # rebuild clears it
        pages.build_pages(wiki, wiki.revision())
        self.assertTrue(pages.verify(wiki)["ok"])

    def test_verify_catches_unresolved_link(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        txn = Transaction(wiki, "concept")
        body = "---\ntype: concept\ndeps:\n  - %s@1\n---\n\nsee [[claim_MISSINGXY]]\n" % claim["id"]
        txn.stage_write("wiki/concepts/db.md", body)
        deps.register(txn, "wiki/concepts/db.md", ["%s@1" % claim["id"]])
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertFalse(v["ok"])
        self.assertTrue(any("link" in e.lower() for e in v["errors"]))

    def test_verify_catches_missing_frontmatter(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        txn = Transaction(wiki, "concept")
        txn.stage_write("wiki/concepts/orphan.md", "no frontmatter here")
        deps.register(txn, "wiki/concepts/orphan.md", [])
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertFalse(v["ok"])
        self.assertTrue(any("frontmatter" in e for e in v["errors"]))

    def test_verify_flags_unregistered_page(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        txn = Transaction(wiki, "concept")
        txn.stage_write("wiki/concepts/unregistered.md",
                        "---\ntype: concept\ndeps: []\n---\n\nbody\n")
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertFalse(v["ok"])
        self.assertTrue(any("registration" in e for e in v["errors"]))


if __name__ == "__main__":
    unittest.main()
