import os, sys, tempfile, unittest
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, deps, doctor, pages, review, sources, synthesize
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "pages-test")
    return wiki

def concept_md(subject="db", body="body text"):
    return "\n".join([
        "---", "type: concept", "subject: %s" % subject,
        "title: %s" % subject.title(), "created: 2026-09-15T00:00:00Z",
        "updated: 2026-09-15", "deps: []", "stale: false", "---",
        "", body, ""])


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

    def test_log_md_built_from_operations_log(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        path = wiki.p("wiki/log.md")
        self.assertTrue(os.path.isfile(path))
        with open(path) as f:
            text = f.read()
        self.assertIn("# Log", text)
        self.assertIn("`wiki-compile`", text)
        self.assertIn("type: log", text)
        with open(wiki.p("wiki/index.md")) as f:
            self.assertIn("[Log](log.md)", f.read())
        self.assertTrue(pages.verify(wiki)["ok"])

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

    def test_verify_unknown_dep_id_errors_no_crash(self):
        # Pinned AND unversioned deps to unknown ids are reported, not a
        # TypeError crash on cur=None.
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        ghost = "claim_" + "B" * 26
        for dep in ("%s@9" % ghost, ghost):
            txn = Transaction(wiki, "concept")
            txn.stage_write("wiki/concepts/ghost.md",
                            "---\ntype: concept\ndeps:\n  - %s\n---\n\nx\n" % dep)
            deps.register(txn, "wiki/concepts/ghost.md", [dep])
            txn.commit(wiki.revision())
            v = pages.verify(wiki)
            self.assertTrue(
                any("unknown dependency id" in e and ghost in e
                    for e in v["errors"]),
                "dep %r: %r" % (dep, v["errors"]))
            txn = Transaction(wiki, "cleanup")
            txn.stage_delete("wiki/concepts/ghost.md")

            def _drop(data):
                data["edges"].pop("wiki/concepts/ghost.md", None)

            txn.stage_state(deps.DEPS_FILE, _drop)
            txn.commit(wiki.revision())

    def test_index_md_checked_like_other_pages(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        ghost = "claim_" + "B" * 26
        txn = Transaction(wiki, "evil")
        txn.stage_write("wiki/index.md",
                        "---\ntype: index\ndeps:\n  - %s@9\n---\n\nx\n" % ghost)
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertTrue(any("index.md" in e for e in v["errors"]), v["errors"])

    def test_review_dep_resolution(self):
        wiki, claim, d = self._wiki_with_claim_and_decision()
        rid = self._add_review_item(wiki, affected=[claim["id"]])
        ghost = "review_" + "B" * 26
        txn = Transaction(wiki, "concept")
        txn.stage_write("wiki/concepts/rv.md",
                        "---\ntype: concept\ndeps:\n  - %s\n---\n\nx\n" % rid)
        deps.register(txn, "wiki/concepts/rv.md", [rid])
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertFalse(any("rv.md" in e for e in v["errors"]), v["errors"])
        for bad in (ghost, "%s@1" % rid, "review_NOTANID!!!"):
            txn = Transaction(wiki, "edit")
            txn.stage_write("wiki/concepts/rv.md",
                            "---\ntype: concept\ndeps:\n  - %s\n---\n\nx\n" % bad)
            txn.commit(wiki.revision())
            v = pages.verify(wiki)
            self.assertTrue(any("unresolved dependency" in e and "rv.md" in e
                                for e in v["errors"]),
                            "dep %r: %r" % (bad, v["errors"]))

    def test_build_pages_replaces_stale_dep_edges(self):
        # Rebuilding a page must REPLACE its dep set: stale edges accumulate
        # forever under merge semantics.
        wiki, claim, d = self._wiki_with_claim_and_decision()
        pages.build_pages(wiki, wiki.revision())
        rel = "wiki/decisions/%s.md" % d["id"]
        txn = Transaction(wiki, "merge-sim")
        deps.register(txn, rel, ["wiki/phantom/ghost.md"])
        txn.commit(wiki.revision())
        pages.build_pages(wiki, wiki.revision())
        edges = deps.graph(wiki)["edges"][rel]
        self.assertNotIn("wiki/phantom/ghost.md", edges)

    def test_page_link_requires_dep_and_resolves(self):
        # page A links [[wiki:concepts/other]]; write fails without the dep
        # and succeeds once 'wiki/concepts/other.md' exists and is in deps
        wiki, claim, d = self._wiki_with_claim_and_decision()
        cdep = "%s@1" % claim["id"]
        synthesize.write_page(wiki, "wiki/concepts/other.md",
                              concept_md(subject="other"), [cdep],
                              wiki.revision())
        md = concept_md(body="see [[wiki:concepts/other]]")
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, "wiki/concepts/db.md", md, [cdep],
                                  wiki.revision())
        r = synthesize.write_page(wiki, "wiki/concepts/db.md", md,
                                  [cdep, "wiki/concepts/other.md"],
                                  wiki.revision())
        self.assertIn("wiki/concepts/other.md", r["deps"])

    def test_page_link_unresolved_rejected(self):
        # [[wiki:concepts/ghost]] with no such file -> TxnError
        wiki, claim, d = self._wiki_with_claim_and_decision()
        cdep = "%s@1" % claim["id"]
        md = concept_md(body="see [[wiki:concepts/ghost]]")
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(wiki, "wiki/concepts/db.md", md, [cdep],
                                  wiki.revision())
        self.assertIn("unresolved [[wiki:", str(ctx.exception))

    def test_verify_flags_unresolved_page_link(self):
        # file exists at write time, dep registered; delete target file ->
        # verify reports "unresolved wiki-link [[wiki:concepts/gone]]"
        wiki, claim, d = self._wiki_with_claim_and_decision()
        cdep = "%s@1" % claim["id"]
        synthesize.write_page(wiki, "wiki/concepts/gone.md",
                              concept_md(subject="gone"), [cdep],
                              wiki.revision())
        md = concept_md(body="see [[wiki:concepts/gone]]")
        synthesize.write_page(wiki, "wiki/concepts/db.md", md,
                              [cdep, "wiki/concepts/gone.md"], wiki.revision())
        os.remove(wiki.p("wiki/concepts/gone.md"))
        v = pages.verify(wiki)
        self.assertFalse(v["ok"])
        self.assertTrue(any("unresolved wiki-link [[wiki:concepts/gone]]" in e
                            for e in v["errors"]), v["errors"])

    def test_page_link_self_rejected(self):
        # a page linking to itself is rejected — self-dep would suppress
        # orphan/dead-end findings and add a self-edge
        wiki, claim, d = self._wiki_with_claim_and_decision()
        cdep = "%s@1" % claim["id"]
        synthesize.write_page(wiki, "wiki/concepts/db.md",
                              concept_md(), [cdep], wiki.revision())
        md = concept_md(body="see [[wiki:concepts/db]]")
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(wiki, "wiki/concepts/db.md", md,
                                  [cdep, "wiki/concepts/db.md"],
                                  wiki.revision())
        self.assertIn("itself", str(ctx.exception))



class FixIndexesTest(unittest.TestCase):
    """rebuild_section_indexes regenerates the index fabric (unlisted_page
    repair) through the build-pages renderer — byte-identical."""

    def test_fix_repairs_unlisted_and_is_idempotent(self):
        wiki = fresh()
        pages.build_pages(wiki, wiki.revision())
        # hand-plant a stray page — NOT via write_page
        stray = wiki.p("wiki", "concepts", "stray-page.md")
        os.makedirs(os.path.dirname(stray), exist_ok=True)
        with open(stray, "w") as f:
            f.write(concept_md(subject="stray page"))

        before = [f["code"] for f in doctor.run(wiki)["findings"]]
        self.assertIn("unlisted_page", before)

        written = pages.rebuild_section_indexes(wiki)
        self.assertEqual(written, sorted(written))
        self.assertIn("wiki/concepts/index.md", written)
        self.assertIn("wiki/overview.md", written)

        after = [f["code"] for f in doctor.run(wiki)["findings"]]
        self.assertNotIn("unlisted_page", after)
        self.assertNotIn("section_index_unlinked", after)
        with open(wiki.p("wiki", "concepts", "index.md")) as f:
            self.assertIn("stray-page.md", f.read())

        # same renderer as build_pages: byte-identical fabric output;
        # _now frozen so `generated` cannot differ across calls
        with mock.patch.object(pages, "_now",
                               return_value="2026-10-25T00:00:00Z"):
            def _bytes(rel):
                with open(wiki.p(rel), "rb") as fh:
                    return fh.read()
            rebuild_a = pages.rebuild_section_indexes(wiki)
            snap = {rel: _bytes(rel) for rel in rebuild_a}
            pages.build_pages(wiki, wiki.revision())
            for rel, blob in snap.items():
                self.assertEqual(
                    blob, _bytes(rel),
                    "%s differs between rebuild and build-pages" % rel)
            # idempotent: a second rebuild emits identical bytes
            pages.rebuild_section_indexes(wiki)
            for rel, blob in snap.items():
                self.assertEqual(blob, _bytes(rel))

if __name__ == "__main__":
    unittest.main()
