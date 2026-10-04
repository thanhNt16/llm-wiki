import argparse, hashlib, io, json, os, sys, tempfile, unittest
from contextlib import redirect_stdout
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, deps, pages, synthesize
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError
from wikicore.yamlite import loads as yamlloads
import wiki as wiki_mod

MISSING = "claim_" + "A" * 26  # well-formed id that never exists


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "synthesize-test")
    return wiki


def save_claim(wiki, subject, predicate="engine", value="postgres", status="accepted",
               authority=None, root_origin=None):
    c = {
        "id": claims.ids.new("claim"),
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "scope": {},
        "valid_from": None,
        "valid_to": None,
        "recorded_at": "2026-09-15T00:00:00Z",
        "status": status,
        "authority": authority or {"type": "independent_corroboration"},
        "evidence": [{"source_id": "source_" + "C" * 26, "source_version": 1,
                      "locator": {"type": "text", "value": "loc"},
                      "root_origin": root_origin or subject}],
        "supersedes": [],
        "version": 1,
    }
    if status in ("rejected", "superseded"):
        c["superseded_by"] = "claim_" + "D" * 26
    txn = Transaction(wiki, "claim")
    claims.save_claim(txn, c)
    txn.commit(wiki.revision())
    return c


def page_md(dep_list, body="We run postgres.", type_="concept", subject="db",
            disputed=None):
    lines = ["---", "type: %s" % type_, "subject: %s" % subject,
             "title: DB engine", "created: 2026-09-15T00:00:00Z",
             "updated: 2026-09-15"]
    if disputed is not None:
        lines.append("disputed: %s" % ("true" if disputed else "false"))
    if dep_list:
        lines.append("deps:")
        for d in dep_list:
            lines.append("  - %s" % d)
    else:
        lines.append("deps: []")
    lines += ["stale: false", "---", "", body, ""]
    return "\n".join(lines)


class TestPageTargets(unittest.TestCase):
    def test_two_claims_two_origins_is_corroborated_target(self):
        wiki = fresh()
        c1 = save_claim(wiki, "db", root_origin="origin-a")
        c2 = save_claim(wiki, "db", predicate="version", value="17", root_origin="origin-b")
        out = synthesize.page_targets(wiki, kind="concepts")
        t = next((x for x in out["targets"] if x["subject"] == "db"), None)
        self.assertIsNotNone(t, out)
        self.assertEqual(t["reason"], "corroborated")
        self.assertEqual(t["artifact"], "wiki/concepts/db.md")
        self.assertEqual(sorted(t["claim_ids"]),
                         sorted("%s@%d" % (c["id"], c["version"]) for c in (c1, c2)))
        self.assertEqual(t["distinct_origins"], 2)
        self.assertEqual(t["claim_count"], 2)

    def test_explicit_decision_is_authoritative_target(self):
        wiki = fresh()
        c = save_claim(wiki, "ci", predicate="vendor", value="gha",
                       authority={"type": "explicit_project_decision"})
        out = synthesize.page_targets(wiki, kind="concepts")
        t = next((x for x in out["targets"] if x["subject"] == "ci"), None)
        self.assertIsNotNone(t, out)
        self.assertEqual(t["reason"], "authoritative")
        self.assertEqual(t["claim_ids"], ["%s@1" % c["id"]])

    def test_single_ordinary_claim_waits(self):
        wiki = fresh()
        save_claim(wiki, "solo")
        out = synthesize.page_targets(wiki, kind="concepts")
        self.assertFalse(any(t["subject"] == "solo" for t in out["targets"]))
        w = next((x for x in out["waiting"] if x["subject"] == "solo"), None)
        self.assertIsNotNone(w, out["waiting"])
        self.assertEqual(w["reason"], "below_threshold")

    def test_candidate_status_not_eligible(self):
        wiki = fresh()
        save_claim(wiki, "db", root_origin="origin-a")
        save_claim(wiki, "db", predicate="version", value="17", status="candidate",
                   root_origin="origin-b")
        out = synthesize.page_targets(wiki, kind="concepts")
        self.assertFalse(any(t["subject"] == "db" for t in out["targets"]))

    def test_invalid_kind_rejected(self):
        wiki = fresh()
        with self.assertRaises(TxnError):
            synthesize.page_targets(wiki, kind="widgets")


class TestWritePage(unittest.TestCase):
    def test_happy_path_writes_registers_and_receipts(self):
        wiki = fresh()
        c1 = save_claim(wiki, "db", root_origin="origin-a")
        c2 = save_claim(wiki, "db", predicate="version", value="17", root_origin="origin-b")
        artifact = "wiki/concepts/db.md"
        dep_list = ["%s@%d" % (c["id"], c["version"]) for c in (c1, c2)]
        body = "We run [[%s]] and [[%s]]." % (c1["id"], c2["id"])
        r = synthesize.write_page(wiki, artifact, page_md(dep_list, body=body), dep_list,
                                  wiki.revision())
        self.assertTrue(os.path.isfile(wiki.p(artifact)))
        self.assertEqual(r["written"], artifact)
        self.assertEqual(sorted(r["deps"]), sorted(dep_list))
        self.assertEqual(r["claims_cited"], 2)
        edges = deps.graph(wiki)["edges"][artifact]
        self.assertEqual(sorted(edges), sorted(dep_list))
        with open(wiki.p(artifact)) as f:
            text = f.read()
        self.assertIn(body, text)
        front, _ = synthesize._split_front(text)
        self.assertEqual(front["type"], "concept")
        self.assertEqual(sorted(front["deps"]), sorted(dep_list))

    def test_bare_dep_id_normalized_to_current_version(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        md = page_md(["%s@1" % c["id"]])
        r = synthesize.write_page(wiki, "wiki/concepts/db.md", md, [c["id"]],
                                  wiki.revision())
        self.assertEqual(r["deps"], ["%s@1" % c["id"]])
        self.assertEqual(deps.graph(wiki)["edges"]["wiki/concepts/db.md"],
                         ["%s@1" % c["id"]])

    def test_write_page_clears_stale(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        artifact = "wiki/concepts/db.md"
        synthesize.write_page(wiki, artifact, page_md(["%s@1" % c["id"]]), [c["id"]],
                              wiki.revision())
        deps.invalidate(wiki, [c["id"]])
        self.assertIn(artifact, deps.stale_artifacts(wiki))
        synthesize.write_page(wiki, artifact, page_md(["%s@1" % c["id"]]), [c["id"]],
                              wiki.revision())
        self.assertNotIn(artifact, deps.stale_artifacts(wiki))

    def _rejects(self, wiki, artifact, md, dep_list):
        before = wiki.revision()
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, artifact, md, dep_list, before)
        self.assertFalse(os.path.isfile(wiki.p(artifact)))
        self.assertNotIn(artifact, deps.graph(wiki).get("edges", {}))
        self.assertEqual(wiki.revision(), before)

    def test_reject_unknown_dep_id(self):
        wiki = fresh()
        self._rejects(wiki, "wiki/concepts/x.md", page_md(["%s@1" % MISSING]), [MISSING])

    def test_reject_dep_version_mismatch(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        txn = Transaction(wiki, "bump")
        c2 = claims.load_claim(wiki, c["id"])
        c2["value"] = "17"
        claims.save_claim(txn, c2)
        txn.commit(wiki.revision())
        self.assertEqual(claims.load_claim(wiki, c["id"])["version"], 2)
        self._rejects(wiki, "wiki/concepts/db.md", page_md(["%s@1" % c["id"]]),
                      ["%s@1" % c["id"]])

    def test_reject_dep_on_rejected_claim(self):
        wiki = fresh()
        c = save_claim(wiki, "dead", status="rejected")
        self._rejects(wiki, "wiki/concepts/dead.md", page_md(["%s@1" % c["id"]]),
                      ["%s@1" % c["id"]])

    def test_reject_reserved_artifacts(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        for artifact in ("wiki/index.md", "wiki/overview.md",
                         "wiki/decisions/decision_X.md", "wiki/concepts/index.md",
                         "../escape.md"):
            self._rejects(wiki, artifact, page_md(dep), dep)

    def test_reject_unresolved_link(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        self._rejects(wiki, "wiki/concepts/db.md",
                      page_md(dep, body="see [[claim_MISSINGXY]]"), dep)

    def test_reject_missing_frontmatter_type(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        self._rejects(wiki, "wiki/concepts/db.md", page_md(dep).replace("type: concept\n", ""),
                      dep)

    def test_reject_type_mismatch(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        self._rejects(wiki, "wiki/concepts/db.md", page_md(dep, type_="entity"), dep)


class TestBuildPagesStale(unittest.TestCase):
    def test_stale_page_keeps_body_and_flag(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        artifact = "wiki/concepts/db.md"
        synthesize.write_page(wiki, artifact,
                              page_md(["%s@1" % c["id"]], body="POSTGRES-ORIGINAL-MARKER"),
                              [c["id"]], wiki.revision())
        deps.invalidate(wiki, [c["id"]])
        pages.build_pages(wiki, wiki.revision())
        path = wiki.p(artifact)
        self.assertTrue(os.path.isfile(path))
        with open(path) as f:
            text = f.read()
        self.assertIn("POSTGRES-ORIGINAL-MARKER", text)
        front, _ = synthesize._split_front(text)
        self.assertTrue(front.get("stale"))
        self.assertIn(artifact, deps.stale_artifacts(wiki))


class TestSectionIndexes(unittest.TestCase):
    def test_build_pages_generates_section_indexes_and_overview(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(["%s@1" % c["id"]]),
                              [c["id"]], wiki.revision())
        pages.build_pages(wiki, wiki.revision())
        for sub in ("concepts",):
            self.assertTrue(os.path.isfile(wiki.p("wiki", sub, "index.md")), sub)
        self.assertTrue(os.path.isfile(wiki.p("wiki", "overview.md")))
        with open(wiki.p("wiki", "index.md")) as f:
            idx = f.read()
        for sub in ("concepts",):
            self.assertIn("%s/index.md" % sub, idx)
        self.assertIn("overview.md", idx)
        v = pages.verify(wiki)
        self.assertTrue(v["ok"], v["errors"])


class TestKindRouting(unittest.TestCase):
    def _rules(self, wiki, data):
        import json as _json
        with open(wiki.p("page-kinds.json"), "w") as f:
            f.write(_json.dumps(data))

    def test_prefix_and_subject_rules(self):
        wiki = fresh()
        self._rules(wiki, {
            "default": "concepts",
            "rules": [
                {"kind": "entities", "prefix": "svc."},
                {"kind": "procedures", "subject": "ops.deploy"},
            ],
        })
        save_claim(wiki, "svc.gateway", authority={"type": "explicit_project_decision"})
        save_claim(wiki, "ops.deploy", authority={"type": "explicit_project_decision"})
        save_claim(wiki, "misc.topic", authority={"type": "explicit_project_decision"})
        out = synthesize.page_targets(wiki)
        kinds = {t["subject"]: t["kind"] for t in out["targets"]}
        self.assertEqual(kinds["svc.gateway"], "entities")
        self.assertEqual(kinds["ops.deploy"], "procedures")
        self.assertEqual(kinds["misc.topic"], "concepts")
        art = {t["subject"]: t["artifact"] for t in out["targets"]}
        self.assertEqual(art["svc.gateway"], "wiki/entities/svc-gateway.md")

    def test_predicate_and_min_claims_rules(self):
        wiki = fresh()
        self._rules(wiki, {"rules": [
            {"kind": "entities", "predicate": "^endpoint$"},
            {"kind": "procedures", "when": {"min_claims": 2}, "regex": "^ops\\."},
        ]})
        save_claim(wiki, "api.charge", predicate="endpoint", value="POST /x",
                   authority={"type": "explicit_project_decision"})
        save_claim(wiki, "ops.rotate", predicate="step", value="a",
                   authority={"type": "explicit_project_decision"})
        out = synthesize.page_targets(wiki)
        kinds = {t["subject"]: t["kind"] for t in out["targets"]}
        self.assertEqual(kinds["api.charge"], "entities")
        # ops.rotate has 1 claim; min_claims=2 rule fails → falls to default
        self.assertEqual(kinds["ops.rotate"], "concepts")

    def test_kind_flag_overrides_rules(self):
        wiki = fresh()
        self._rules(wiki, {"rules": [{"kind": "entities", "prefix": "svc."}]})
        save_claim(wiki, "svc.gateway", authority={"type": "explicit_project_decision"})
        out = synthesize.page_targets(wiki, kind="questions")
        self.assertEqual(out["targets"][0]["kind"], "questions")

    def test_malformed_rules_file_warns_and_defaults(self):
        wiki = fresh()
        with open(wiki.p("page-kinds.json"), "w") as f:
            f.write("not json{")
        save_claim(wiki, "x.y", authority={"type": "explicit_project_decision"})
        out = synthesize.page_targets(wiki)
        self.assertEqual(out["targets"][0]["kind"], "concepts")
        self.assertTrue(out["warnings"])

    def test_invalid_rule_kind_ignored(self):
        wiki = fresh()
        self._rules(wiki, {"rules": [{"kind": "bogus", "subject": "x.y"},
                                     {"kind": "entities", "subject": "x.y"}]})
        save_claim(wiki, "x.y", authority={"type": "explicit_project_decision"})
        out = synthesize.page_targets(wiki)
        self.assertEqual(out["targets"][0]["kind"], "entities")
        self.assertEqual(out["targets"][0]["matched_rule"], 1)

def save_decision(wiki):
    d = {
        "id": claims.ids.new("decision"),
        "title": "Use Postgres", "status": "accepted", "decided_on": "2026-01-10",
        "recorded_at": "2026-09-15T00:00:00Z", "version": 1, "claims": [],
        "evidence": [{"source_id": "source_" + "C" * 26, "source_version": 1,
                      "locator": {"type": "text", "value": "x"}}],
        "body": "",
    }
    txn = Transaction(wiki, "decision")
    claims.save_decision(txn, d)
    txn.commit(wiki.revision())
    return d


class TestSlugContract(unittest.TestCase):
    def test_slug_mismatch_rejected(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        self._rejects(wiki, "wiki/concepts/db-engine.md", page_md(dep), dep)

    def test_collision_suffix_artifact_accepted(self):
        wiki = fresh()
        subject = "a.b"
        c = save_claim(wiki, subject)
        dep = ["%s@1" % c["id"]]
        slug = "a-b-%s" % hashlib.md5(subject.encode("utf-8")).hexdigest()[:6]
        r = synthesize.write_page(wiki, "wiki/concepts/%s.md" % slug,
                                  page_md(dep, subject=subject), dep, wiki.revision())
        self.assertEqual(r["written"], "wiki/concepts/%s.md" % slug)

    def test_missing_subject_title_rejected(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        self._rejects(wiki, "wiki/concepts/db.md",
                      page_md(dep).replace("subject: db\n", ""), dep)
        self._rejects(wiki, "wiki/concepts/db.md",
                      page_md(dep).replace("title: DB engine\n", ""), dep)

    def test_date_only_created_rejected_on_new_page(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        self._rejects(wiki, "wiki/concepts/db.md",
                      page_md(dep).replace("created: 2026-09-15T00:00:00Z",
                                           "created: 2026-09-15"), dep)

    def test_refresh_backfills_created(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        artifact = "wiki/concepts/db.md"
        synthesize.write_page(wiki, artifact, page_md(dep), dep, wiki.revision())
        stripped = "\n".join(l for l in page_md(dep).splitlines()
                             if not l.startswith("created:"))
        r = synthesize.write_page(wiki, artifact, stripped, dep, wiki.revision())
        self.assertEqual(r["written"], artifact)
        with open(wiki.p(artifact)) as f:
            front, _ = synthesize._split_front(f.read())
        self.assertEqual(front["created"], "2026-09-15T00:00:00Z")

    def test_subject_immutable_on_refresh(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        artifact = "wiki/concepts/db.md"
        synthesize.write_page(wiki, artifact, page_md(dep), dep, wiki.revision())
        self._rejects(wiki, artifact, page_md(dep, subject="other"), dep)

    def _rejects(self, wiki, artifact, md, dep_list):
        before = wiki.revision()
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, artifact, md, dep_list, before)
        self.assertEqual(wiki.revision(), before)


class TestSlugCollisions(unittest.TestCase):
    def _two_targets(self, s1, s2):
        wiki = fresh()
        save_claim(wiki, s1, authority={"type": "explicit_project_decision"})
        save_claim(wiki, s2, authority={"type": "explicit_project_decision"})
        return synthesize.page_targets(wiki)

    def test_punct_variants_collide(self):
        out = self._two_targets("a.b", "a_b")
        slugs = sorted(t["slug"] for t in out["targets"])
        self.assertEqual(slugs[0], "a-b")
        self.assertTrue(slugs[1].startswith("a-b-") and slugs[1] != "a-b")
        self.assertTrue(any("collision" in w for w in out["warnings"]))

    def test_unicode_only_subjects_hit_untitled(self):
        out = self._two_targets("日本語", "日本語！")
        slugs = sorted(t["slug"] for t in out["targets"])
        self.assertEqual(slugs[0], "untitled")
        self.assertTrue(slugs[1].startswith("untitled-"))

    def test_long_subject_truncation_collides(self):
        out = self._two_targets("x" * 70 + " one", "x" * 70 + " two")
        slugs = sorted(t["slug"] for t in out["targets"])
        self.assertEqual(slugs[0], "x" * 60)
        self.assertTrue(slugs[1].startswith("x" * 60 + "-"))


class TestTypedDeps(unittest.TestCase):
    def test_decision_dep_accepted_and_version_checked(self):
        wiki = fresh()
        d = save_decision(wiki)
        dep = "%s@1" % d["id"]
        r = synthesize.write_page(wiki, "wiki/concepts/db.md", page_md([dep]),
                                  [dep], wiki.revision())
        self.assertEqual(r["deps"], [dep])
        self._rejects(wiki, "wiki/concepts/db.md", page_md(["%s@2" % d["id"]]),
                      ["%s@2" % d["id"]])

    def test_wiki_page_dep_and_missing_page_rejected(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = ["%s@1" % c["id"]]
        synthesize.write_page(wiki, "wiki/concepts/a.md", page_md(dep, subject="a"),
                              dep, wiki.revision())
        r = synthesize.write_page(wiki, "wiki/concepts/b.md",
                                  page_md(["wiki/concepts/a.md"], subject="b"),
                                  ["wiki/concepts/a.md"], wiki.revision())
        self.assertEqual(r["deps"], ["wiki/concepts/a.md"])
        self.assertEqual(deps.graph(wiki)["edges"]["wiki/concepts/b.md"],
                         ["wiki/concepts/a.md"])
        self._rejects(wiki, "wiki/concepts/c.md",
                      page_md(["wiki/concepts/nope.md"], subject="c"),
                      ["wiki/concepts/nope.md"])

    def _rejects(self, wiki, artifact, md, dep_list):
        before = wiki.revision()
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, artifact, md, dep_list, before)


class TestWritePageGates(unittest.TestCase):
    def test_disputed_dep_requires_flag(self):
        wiki = fresh()
        c = save_claim(wiki, "db", status="disputed")
        dep = ["%s@1" % c["id"]]
        before = wiki.revision()
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep), dep, before)
        self.assertEqual(wiki.revision(), before)
        r = synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep, disputed=True),
                                  dep, wiki.revision())
        self.assertEqual(r["deps"], dep)

    def test_linked_claims_must_be_in_deps(self):
        wiki = fresh()
        c1 = save_claim(wiki, "db")
        c2 = save_claim(wiki, "db2")
        md = page_md(["%s@1" % c1["id"]], body="We run [[%s]]." % c2["id"])
        before = wiki.revision()
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, "wiki/concepts/db.md", md,
                                  ["%s@1" % c1["id"]], before)
        self.assertEqual(wiki.revision(), before)

    def test_question_page_allows_empty_deps(self):
        wiki = fresh()
        r = synthesize.write_page(wiki, "wiki/questions/how.md",
                                  page_md([], type_="question", subject="how"),
                                  [], wiki.revision())
        self.assertEqual(r["deps"], [])

    def test_non_question_empty_deps_rejected(self):
        wiki = fresh()
        before = wiki.revision()
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, "wiki/concepts/db.md", page_md([]), [], before)
        self.assertEqual(wiki.revision(), before)

    def test_auto_derive_from_links(self):
        wiki = fresh()
        c1 = save_claim(wiki, "db")
        c2 = save_claim(wiki, "db2")
        md = page_md(None, body="We run [[%s]] and [[%s]]." % (c1["id"], c2["id"]))
        r = synthesize.write_page(wiki, "wiki/concepts/db.md", md, None,
                                  wiki.revision())
        self.assertEqual(sorted(r["deps"]),
                         sorted(["%s@1" % c1["id"], "%s@1" % c2["id"]]))


class TestWritePageCli(unittest.TestCase):
    def _run(self, wiki, tmp, md, **kw):
        page = os.path.join(tmp, "page.md")
        with open(page, "w") as f:
            f.write(md)
        ns = argparse.Namespace(root=wiki.root, file=kw.get("file", page),
                                text=kw.get("text"), artifact=kw["artifact"],
                                deps_file=kw.get("deps_file"), deps=kw.get("deps"),
                                base_revision=None)
        out = io.StringIO()
        with redirect_stdout(out):
            wiki_mod.cmd_write_page(ns)
        return json.loads(out.getvalue())

    def test_stdin_write(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = "%s@1" % c["id"]
        md = page_md([dep], body="We run [[%s]]." % c["id"])
        with mock.patch("sys.stdin", io.StringIO(md)):
            r = self._run(wiki, tempfile.mkdtemp(), "", file="-", artifact="wiki/concepts/db.md",
                          deps=dep)
        self.assertEqual(r["written"], "wiki/concepts/db.md")
        self.assertTrue(os.path.isfile(wiki.p("wiki/concepts/db.md")))

    def test_deps_file_page_target_dict(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = "%s@1" % c["id"]
        tmp = tempfile.mkdtemp()
        targets = os.path.join(tmp, "targets.json")
        with open(targets, "w") as f:
            json.dump({"subject": "db", "kind": "concepts", "slug": "db",
                       "artifact": "wiki/concepts/db.md", "claim_ids": [dep]}, f)
        r = self._run(wiki, tmp, page_md([dep]), artifact="wiki/concepts/db.md",
                      deps_file=targets)

    def test_no_deps_anywhere_auto_derives(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        md = page_md(None, body="We run [[%s]]." % c["id"])
        md = "\n".join(l for l in md.splitlines() if l != "deps: []")
        r = self._run(wiki, tempfile.mkdtemp(), md, artifact="wiki/concepts/db.md",
                      file=None, text=md)
        self.assertEqual(r["deps"], ["%s@1" % c["id"]])

    def test_page_show_reports_front_body_details(self):
        wiki = fresh()
        c = save_claim(wiki, "db")
        dep = "%s@1" % c["id"]
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md([dep]), [dep],
                              wiki.revision())
        out = io.StringIO()
        ns = argparse.Namespace(root=wiki.root, artifact="wiki/concepts/db.md")
        with redirect_stdout(out):
            wiki_mod.cmd_page_show(ns)
        shown = json.loads(out.getvalue())
        self.assertEqual(shown["artifact"], "wiki/concepts/db.md")
        self.assertEqual(shown["front"]["subject"], "db")
        self.assertIn("We run postgres.", shown["body"])
        self.assertEqual(shown["claim_details"][0]["id"], c["id"])
        self.assertFalse(shown["stale"])
        self.assertEqual(shown["deps_registered"], [dep])


if __name__ == "__main__":
    unittest.main()
