import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, deps, pages, review, sources
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

    def _add_review_item(self, wiki, affected=()):
        txn = Transaction(wiki, "ri")
        rid = review.add_item(txn, "authority_conflict", "which value wins",
                              [], list(affected), "high")
        txn.commit(wiki.revision())
        return rid

    def test_overview_and_index_get_new_sections(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        rid = self._add_review_item(wiki, affected=[claim["id"]])
        pages.build_pages(wiki, wiki.revision())
        with open(wiki.p("wiki/overview.md")) as f:
            ov = f.read()
        for heading in ("## Review queue", "## Stale pages", "## Disputed claims",
                        "## Superseded claims", "## Orphan claims",
                        "## Coverage gaps", "## Synthesis targets"):
            self.assertIn(heading, ov)
        self.assertIn(rid, ov)
        self.assertIn(claim["id"], ov)
        with open(wiki.p("wiki/review.md")) as f:
            rq = f.read()
        self.assertIn(rid, rq)
        self.assertIn("authority_conflict", rq)
        self.assertIn("high", rq)
        self.assertIn("[[%s]]" % claim["id"], rq)
        with open(wiki.p("wiki/index.md")) as f:
            idx = f.read()
        self.assertIn("| Subject | Live statuses | Disputed | Open reviews |", idx)
        self.assertIn("[Review queue](review.md)", idx)

    def test_changes_index_built_from_operations_log(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        path = wiki.p("wiki/changes/index.md")
        self.assertTrue(os.path.isfile(path))
        with open(path) as f:
            text = f.read()
        self.assertIn("| Revision | At | Op | Run | Affected |", text)
        self.assertIn("wiki-compile", text)
        self.assertIn("wiki-ingest", text)

    def test_verify_accepts_current_versions_and_wiki_paths(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        txn = Transaction(wiki, "concept")
        body = ("---\ntype: concept\ndeps:\n  - %s@%d\n  - wiki/overview.md\n---\n"
                "\n\nbody\n" % (claim["id"], claim["version"]))
        txn.stage_write("wiki/concepts/ok.md", body)
        deps.register(txn, "wiki/concepts/ok.md",
                      ["%s@%d" % (claim["id"], claim["version"])])
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertTrue(v["ok"], v["errors"])

    def test_verify_catches_stale_version_dep(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        txn = Transaction(wiki, "concept")
        body = "---\ntype: concept\ndeps:\n  - %s@1\n---\n\nbody\n" % claim["id"]
        txn.stage_write("wiki/concepts/stale.md", body)
        deps.register(txn, "wiki/concepts/stale.md", ["%s@1" % claim["id"]])
        txn.commit(wiki.revision())
        txn = Transaction(wiki, "bump")
        c = claims.load_claim(wiki, claim["id"])
        c["value"] = c["value"]
        claims.save_claim(txn, c)  # bumps version past the pinned @1
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertFalse(v["ok"])
        self.assertTrue(any("does not match current version" in e
                            for e in v["errors"]))

    def test_verify_rejects_missing_wiki_dep(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        txn = Transaction(wiki, "concept")
        body = "---\ntype: concept\ndeps:\n  - wiki/nope/missing.md\n---\n\nbody\n"
        txn.stage_write("wiki/concepts/bad.md", body)
        deps.register(txn, "wiki/concepts/bad.md", ["wiki/nope/missing.md"])
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertFalse(v["ok"])
        self.assertTrue(any("wiki/nope/missing.md" in e for e in v["errors"]))


if __name__ == "__main__":
    unittest.main()
