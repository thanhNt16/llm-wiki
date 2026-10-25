import argparse, hashlib, io, json, os, sys, tempfile, unittest
from contextlib import redirect_stdout
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from wikicore import claims, deps, pages, synthesize, yamlite
from wikicore import topics
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError
import wiki as wiki_mod
from test_doctor import add_accepted_claim

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

    def test_shared_origin_does_not_double_count(self):
        # Two claims tracing to the SAME root origin count once: corroboration
        # is distinct origins across the claim set, not summed per claim.
        wiki = fresh()
        save_claim(wiki, "db", root_origin="same-origin")
        save_claim(wiki, "db", predicate="version", value="17",
                   root_origin="same-origin")
        out = synthesize.page_targets(wiki, kind="concepts")
        self.assertFalse(any(t["subject"] == "db" for t in out["targets"]))
        w = next((x for x in out["waiting"] if x["subject"] == "db"), None)
        self.assertIsNotNone(w, out["waiting"])
        self.assertEqual(w["distinct_origins"], 1)

    def test_single_source_open_question_is_target(self):
        # an open_question claim promotes its subject to targets at
        # min_claims:1, exempt from BOTH corroboration thresholds
        wiki = fresh()
        save_claim(wiki, "deploy.rollback", predicate="open_question",
                   value="who owns the rollback path?",
                   root_origin="only-origin")
        out = synthesize.page_targets(wiki)
        t = next((x for x in out["targets"] if x["subject"] == "deploy.rollback"),
                 None)
        self.assertIsNotNone(t, out)
        self.assertEqual(t["reason"], "open_question")
        self.assertEqual(t["kind"], "questions")
        self.assertEqual(t["claim_count"], 1)
        self.assertEqual(t["distinct_origins"], 1)
        self.assertFalse(any(w["subject"] == "deploy.rollback"
                             for w in out["waiting"]))

    def test_corroborated_open_question_subject_unchanged(self):
        # a subject already past both thresholds keeps reason "corroborated"
        # even when one of its claims is an open_question
        wiki = fresh()
        save_claim(wiki, "deploy.rollback", predicate="open_question",
                   value="who owns the rollback path?", root_origin="origin-a")
        save_claim(wiki, "deploy.rollback", predicate="owner", value="sre",
                   root_origin="origin-b")
        out = synthesize.page_targets(wiki)
        t = next((x for x in out["targets"] if x["subject"] == "deploy.rollback"),
                 None)
        self.assertIsNotNone(t, out)
        self.assertEqual(t["reason"], "corroborated")
        self.assertEqual(t["claim_count"], 2)
        self.assertEqual(t["distinct_origins"], 2)



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

    def test_stale_body_with_hr_survives(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        artifact = "wiki/concepts/db.md"
        synthesize.write_page(wiki, artifact,
                              page_md(["%s@1" % c["id"]],
                                      body="HR-BEFORE\n\n---\n\nHR-AFTER"),
                              [c["id"]], wiki.revision())
        deps.invalidate(wiki, [c["id"]])
        pages.build_pages(wiki, wiki.revision())
        with open(wiki.p(artifact)) as f:
            text = f.read()
        self.assertIn("HR-BEFORE", text)
        self.assertIn("HR-AFTER", text)
        front, body = synthesize._split_front(text)
        self.assertTrue(front.get("stale"))
        self.assertIn("---", body)

    def test_stale_frontmatter_value_with_dashes_survives(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        artifact = "wiki/concepts/db.md"
        md = page_md(["%s@1" % c["id"]], body="DASH-BODY").replace(
            "title: DB engine", "title: a---b")
        synthesize.write_page(wiki, artifact, md, [c["id"]], wiki.revision())
        deps.invalidate(wiki, [c["id"]])
        pages.build_pages(wiki, wiki.revision())
        with open(wiki.p(artifact)) as f:
            text = f.read()
        front, body = synthesize._split_front(text)
        self.assertEqual(front.get("title"), "a---b")
        self.assertIn("DASH-BODY", body)



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
        # ops.rotate has 1 claim; min_claims=2 file rule fails -> falls
        # through to heuristics, which match the `step` predicate (rule 1)
        self.assertEqual(kinds["ops.rotate"], "procedures")

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
                                  page_md(["wiki/concepts/a.md"], subject="b")
                                  .replace("title: DB engine", "title: B page"),
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
                                allow_new=kw.get("allow_new", False),
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


class TestPageShowSecurity(unittest.TestCase):
    def _page(self, wiki):
        c = save_claim(wiki, "db")
        dep = "%s@1" % c["id"]
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md([dep]), [dep],
                              wiki.revision())

    def test_traversal_rejected(self):
        wiki = fresh()
        self._page(wiki)
        for artifact in ("wiki/concepts/../../wiki.json",
                         "wiki/concepts/../../../etc/passwd",
                         "/etc/passwd", "wiki//concepts/db.md",
                         "README.md"):
            with self.assertRaises(TxnError, msg=artifact):
                synthesize.page_show(wiki, artifact)

    def test_unregistered_wiki_path_rejected(self):
        wiki = fresh()
        self._page(wiki)
        txn = Transaction(wiki, "manual")
        txn.stage_write("wiki/concepts/unreg.md",
                        "---\ntype: concept\n---\n\nbody\n")
        txn.commit(wiki.revision())
        with self.assertRaises(TxnError):
            synthesize.page_show(wiki, "wiki/concepts/unreg.md")


class TestYamliteRoundTrip(unittest.TestCase):
    def _rt(self, data):
        return yamlite.loads("---\n" + yamlite.dumps(data) + "\n---\n")

    def test_typelike_strings_stay_strings(self):
        d = {"a": "123", "b": "true", "c": "false", "d": "null",
             "e": "1.5", "f": "007", "g": "-4", "h": "[]"}
        self.assertEqual(self._rt(d), d)

    def test_newline_escaped_no_injection(self):
        out = self._rt({"subject": "db\nhacked: true"})
        self.assertEqual(out, {"subject": "db\nhacked: true"})
        self.assertNotIn("hacked", out)

    def test_backslash_and_quotes_round_trip(self):
        d = {"a": "path\\to 'x'\nline2", "b": "a: b # c", "c": "ends "}
        self.assertEqual(self._rt(d), d)

    def test_list_item_under_scalar_raises_txnerror(self):
        with self.assertRaises(TxnError):
            yamlite.loads("---\na: 1\n  - x\n---\n")

    def test_inline_fence_in_value_not_terminator(self):
        # "---" mid-value must not close the frontmatter block.
        front, body = synthesize._split_front(
            "---\ntitle: a---b\n---\n\n---\n\nbody-after\n")
        self.assertEqual(front.get("title"), "a---b")
        self.assertIn("body-after", body)


class SimilarPagesTest(unittest.TestCase):
    def setUp(self):
        # two accepted claims, subject "attribution window", distinct root
        # origins -> a corroborated page target
        self.wiki = fresh()
        save_claim(self.wiki, "attribution window", predicate="length",
                   value="7d", root_origin="sim-src-1")
        save_claim(self.wiki, "attribution window", predicate="length",
                   value="30d", root_origin="sim-src-2")

    def _page_md(self):
        cl = claims.list_claims(self.wiki, subject="attribution window")
        return ("---\ntype: concept\nsubject: attribution window\n"
                "title: Attribution Window\n"
                "created: 2026-09-15T00:00:00Z\n---\n\n# Attribution Window\n\n"
                + "\n".join("[[%s]]" % c["id"] for c in cl))

    def _plant_page(self):
        return synthesize.write_page(
            self.wiki, "wiki/concepts/attribution-window.md",
            self._page_md(), None, self.wiki.revision())

    def test_similar_pages_finds_near_duplicate(self):
        self._plant_page()
        sim = synthesize.similar_pages(self.wiki, "concepts",
                                       "attribution window backup",
                                       "Attribution Window Backup")
        self.assertTrue(any(s["similarity"] >= 0.6 for s in sim))
        sim_far = synthesize.similar_pages(self.wiki, "concepts",
                                           "retry policy", "Retry Policy")
        self.assertEqual(sim_far, [])

    def test_write_page_refuses_near_duplicate_new_page(self):
        self._plant_page()
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(
                self.wiki, "wiki/concepts/attribution-window-backup.md",
                self._page_md().replace(
                    "subject: attribution window",
                    "subject: attribution window backup").replace(
                    "title: Attribution Window",
                    "title: Attribution Window Backup"),
                None, self.wiki.revision())
        self.assertIn("attribution-window.md", str(ctx.exception))

    def test_write_page_allow_new_overrides(self):
        self._plant_page()
        out = synthesize.write_page(
            self.wiki, "wiki/concepts/attribution-window-backup.md",
            self._page_md().replace(
                "subject: attribution window",
                "subject: attribution window backup").replace(
                "title: Attribution Window",
                "title: Attribution Window Backup"),
            None, self.wiki.revision(), allow_new=True)
        self.assertTrue(out)
        with open(self.wiki.p(
                "wiki/concepts/attribution-window-backup.md")) as f:
            self.assertIn("forced_new: true", f.read())

    def test_page_targets_emit_similar_existing(self):
        self._plant_page()
        save_claim(self.wiki, "attribution window backup", predicate="length",
                   value="7d", root_origin="sim-src-1")
        save_claim(self.wiki, "attribution window backup", predicate="length",
                   value="30d", root_origin="sim-src-2")
        targets = synthesize.page_targets(self.wiki)
        t = [x for x in targets["targets"]
             if x["subject"] == "attribution window backup"]
        self.assertTrue(t)
        self.assertIn("similar_existing", t[0])
        self.assertEqual(t[0]["routing"], "extend-first")
        # guard routes NEW pages extend-first; existing pages are refreshed
        existing = [x for x in targets["targets"]
                    if x["subject"] == "attribution window"][0]
        self.assertNotIn("similar_existing", existing)

    def test_write_page_cli_allow_new_flag(self):
        self._plant_page()
        md = self._page_md().replace(
            "subject: attribution window",
            "subject: attribution window backup").replace(
            "title: Attribution Window",
            "title: Attribution Window Backup")
        argv = ["--root", self.wiki.root, "write-page", "--text", md,
                "--artifact", "wiki/concepts/attribution-window-backup.md"]
        out = io.StringIO()
        with redirect_stdout(out):
            rc = wiki_mod.main(argv)
        self.assertEqual(rc, 3)  # near-duplicate refusal -> TxnError -> exit 3
        with redirect_stdout(out):
            rc = wiki_mod.main(argv + ["--allow-new"])
        self.assertEqual(rc, 0)
        with open(self.wiki.p(
                "wiki/concepts/attribution-window-backup.md")) as f:
            self.assertIn("forced_new: true", f.read())


class UnifiedWritePageTest(unittest.TestCase):
    # NOTE (brief deviation): the brief's snippet used subject "other subject"
    # against artifact wiki/concepts/other.md — slugify("other subject") is
    # "other-subject", so the pre-existing slug guard fires before the link
    # checks and the asserts can never see the intended errors. Subject "other"
    # is the slug-matching form; everything else is verbatim.
    def setUp(self):
        self.wiki = fresh()
        for i in (1, 2):
            add_accepted_claim(self.wiki, "attribution window", "length", "%dd" % (7 if i == 1 else 30))

    def _md(self, subject, title, links=()):
        cl = claims.list_claims(self.wiki, subject="attribution window")
        body = "\n".join("[[%s]]" % c["id"] for c in cl)
        if links:
            body += "\n\n## See also\n\n" + "\n".join(links)
        return ("---\ntype: concept\nsubject: %s\ntitle: %s\n"
                "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
                % (subject, title, title, body))

    def test_guard_and_link_validation_coexist(self):
        synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                              self._md("attribution window", "Attribution Window"),
                              None, self.wiki.revision())
        # near-dup refusal message names the twin AND the remediation
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(
                self.wiki, "wiki/concepts/attribution-window-backup.md",
                self._md("attribution window backup", "Attribution Window Backup"),
                None, self.wiki.revision())
        msg = str(ctx.exception)
        self.assertIn("wiki/concepts/attribution-window.md", msg)
        self.assertIn("--allow-new", msg)
        # self-link rejected even with --allow-new
        with self.assertRaises(TxnError) as ctx2:
            synthesize.write_page(
                self.wiki, "wiki/concepts/other.md",
                self._md("other", "Other",
                         links=["[[wiki:concepts/other]]"]),
                None, self.wiki.revision(), allow_new=True)
        self.assertIn("self-link", str(ctx2.exception).lower())

    def test_page_link_requires_existing_target_and_dep(self):
        synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                              self._md("attribution window", "Attribution Window"),
                              None, self.wiki.revision())
        # broken target
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(
                self.wiki, "wiki/concepts/other.md",
                self._md("other", "Other", links=["[[wiki:concepts/nope]]"]),
                None, self.wiki.revision(), allow_new=True)
        self.assertIn("nope", str(ctx.exception))


class UnifiedGuardSurfaceTest(unittest.TestCase):
    """New module surface: threshold constants, extract_page_links, page-link
    auto-derive, and the unified refusal message shape."""

    def test_threshold_constants(self):
        self.assertEqual(synthesize.NEAR_DUP_WRITE_THRESHOLD, 0.60)
        self.assertEqual(synthesize.NEAR_DUP_LINT_THRESHOLD, 0.70)

    def test_extract_page_links_dedup_first_seen_order(self):
        body = ("[[wiki:concepts/b]] mid [[wiki:concepts/a]] "
                "[[wiki:concepts/b]] tail [[wiki:entities/x-y]]")
        self.assertEqual(synthesize.extract_page_links(body),
                         ["concepts/b", "concepts/a", "entities/x-y"])
        self.assertEqual(synthesize.extract_page_links("no links here"), [])

    def test_auto_derive_includes_page_link_deps(self):
        wiki = fresh()
        ca = save_claim(wiki, "a")
        cb = save_claim(wiki, "b")
        synthesize.write_page(wiki, "wiki/concepts/a.md",
                              page_md(["%s@1" % ca["id"]], subject="a"),
                              ["%s@1" % ca["id"]], wiki.revision())
        md = ("---\ntype: concept\nsubject: b\ntitle: B page\n"
              "created: 2026-09-15T00:00:00Z\n---\n\n"
              "See [[wiki:concepts/a]] and [[%s]].\n" % cb["id"])
        r = synthesize.write_page(wiki, "wiki/concepts/b.md", md, None,
                                  wiki.revision())
        self.assertEqual(sorted(r["deps"]),
                         sorted(["%s@1" % cb["id"], "wiki/concepts/a.md"]))

    def test_unified_refusal_message_shape(self):
        wiki = fresh()
        save_claim(wiki, "attribution window", predicate="length",
                   value="7d", root_origin="o1")
        save_claim(wiki, "attribution window", predicate="length",
                   value="30d", root_origin="o2")
        cl = claims.list_claims(wiki, subject="attribution window")
        body = "\n".join("[[%s]]" % c["id"] for c in cl)
        md = ("---\ntype: concept\nsubject: attribution window\n"
              "title: Attribution Window\n"
              "created: 2026-09-15T00:00:00Z\n---\n\n# Attribution Window\n\n%s\n"
              % body)
        synthesize.write_page(wiki, "wiki/concepts/attribution-window.md", md,
                              None, wiki.revision())
        backup = (md.replace("subject: attribution window",
                             "subject: attribution window backup")
                   .replace("title: Attribution Window",
                            "title: Attribution Window Backup")
                   .replace("# Attribution Window",
                            "# Attribution Window Backup"))
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(wiki, "wiki/concepts/attribution-window-backup.md",
                                  backup, None, wiki.revision())
        msg = str(ctx.exception)
        self.assertTrue(msg.startswith("near-duplicate page: "), msg)
        self.assertIn("(sim=", msg)
        self.assertIn("title='Attribution Window')", msg)
        self.assertIn("already covers this — extend it, or pass --allow-new", msg)
        self.assertIn("twins: wiki/concepts/attribution-window.md", msg)
        # forcing past twins persists the audit flag
        r = synthesize.write_page(wiki, "wiki/concepts/attribution-window-backup.md",
                                  backup, None, wiki.revision(), allow_new=True)
        self.assertTrue(r)
        with open(wiki.p("wiki/concepts/attribution-window-backup.md")) as f:
            self.assertIn("forced_new: true", f.read())
        # --allow-new on a page with NO twins records no override
        cc = save_claim(wiki, "c")
        md_c = ("---\ntype: concept\nsubject: c\ntitle: C page\n"
                "created: 2026-09-15T00:00:00Z\n---\n\n[[%s]]\n" % cc["id"])
        synthesize.write_page(wiki, "wiki/concepts/c.md", md_c, None,
                              wiki.revision(), allow_new=True)
        with open(wiki.p("wiki/concepts/c.md")) as f:
            self.assertNotIn("forced_new", f.read())


def _naive_all_pages_scan(wiki, subject, title, threshold):
    """Pre-index similar_pages semantics: every page's title/subject tokens
    Jaccard-compared against the want set; exact-title match also counts.
    Returns {(section, slug)}."""
    want = synthesize._norm_title_tokens(title) | \
        synthesize._norm_title_tokens(subject)
    out = set()
    for sec in synthesize.PAGE_KINDS:
        d = wiki.p("wiki", sec)
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if not name.endswith(".md") or name == "index.md":
                continue
            with open(wiki.p("wiki", sec, name), encoding="utf-8") as f:
                front, _ = synthesize._split_front(f.read())
            have = synthesize._norm_title_tokens(str(front.get("title") or "")) \
                | synthesize._norm_title_tokens(str(front.get("subject") or ""))
            if synthesize._jaccard(want, have) >= threshold or (
                    str(front.get("title") or "").strip().lower()
                    == (title or "").strip().lower()):
                out.add((sec, name[:-3]))
    return out


def _plant_page(wiki, sec, slug, subject, title):
    """Write a minimal page straight to disk (similar_pages only reads the
    filesystem — write_page's guard would refuse planted near-dups)."""
    d = wiki.p("wiki", sec)
    os.makedirs(d, exist_ok=True)
    with open(wiki.p("wiki", sec, slug + ".md"), "w") as f:
        f.write("---\ntype: concept\nsubject: %s\ntitle: %s\n"
                "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n" % (subject, title,
                                                                title))


class InvertedIndexEquivalenceTest(unittest.TestCase):
    """Task 6: the inverted token index enumerates exactly the token-sharing
    pairs — lossless vs the naive O(N^2) scan it replaces."""

    def test_indexed_equals_naive(self):
        import random
        rng = random.Random(42)
        entries = {"p%d" % i: set(rng.sample(
            ["attr", "window", "retry", "order", "cache", "token", "graph",
             "page", "link", "stale"], rng.randint(1, 4)))
            for i in range(60)}
        naive = {(a, b) for i, a in enumerate(sorted(entries))
                 for b in sorted(entries)[i + 1:]
                 if entries[a] & entries[b]}
        idx = synthesize.build_token_index(entries)
        # index posting lists are sorted (doctor relies on pair order)
        for keys in idx.values():
            self.assertEqual(keys, sorted(keys))
        self.assertEqual(naive, synthesize.candidate_pairs(idx))

    def test_similar_pages_matches_naive_scan(self):
        wiki = fresh()
        rng = __import__("random").Random(7)
        vocab = ["attr", "window", "retry", "order", "cache", "token",
                 "graph", "page", "link", "stale", "quota", "shard",
                 "lease", "epoch", "vector", "cursor", "merkle", "fence"]
        # 30 pages, ~50% vocab overlap between neighbours plus pages that
        # share zero tokens with the probe — exercises the index skip path.
        for i in range(30):
            subject = " ".join(rng.sample(vocab, rng.randint(1, 4)))
            _plant_page(wiki, "concepts", "page-%02d" % i, subject,
                        subject.title())
        _plant_page(wiki, "entities", "probe", "attr window",
                    "Attr Window")
        # probe A: real tokens -> hits plus zero-token pages skipped
        want = _naive_all_pages_scan(wiki, "attr window", "Attr Window", 0.60)
        got = {(t["section"], t["slug"]) for t in synthesize.similar_pages(
            wiki, "concepts", "attr window", "Attr Window")}
        self.assertEqual(want, got)
        self.assertIn(("entities", "probe"), got)  # exact-title path
        # probe B: no tokens at all -> only exact-title matches can appear
        want = _naive_all_pages_scan(wiki, "zzz", "zzz", 0.60)
        got = {(t["section"], t["slug"]) for t in synthesize.similar_pages(
            wiki, "concepts", "zzz", "zzz")}
        self.assertEqual(want, got)
        self.assertEqual(got, set())


class HeuristicRoutingTest(unittest.TestCase):
    """Built-in kind routing: active when page-kinds.json is absent, unparseable,
    or carries no usable rules; file rules always beat heuristics."""

    def _row(self, wiki, subject):
        out = synthesize.page_targets(wiki)
        return next((t for t in out["targets"] if t["subject"] == subject), None)

    def _rules(self, wiki, data):
        with open(wiki.p("page-kinds.json"), "w") as f:
            f.write(json.dumps(data))

    def test_ticket_subject_routes_entity(self):
        wiki = fresh()  # init writes {"rules": []} -> heuristics active
        save_claim(wiki, "stc-88", predicate="status", value="shipped",
                   root_origin="origin-1")
        save_claim(wiki, "stc-88", predicate="root cause", value="x",
                   root_origin="origin-2")
        row = self._row(wiki, "stc-88")
        self.assertIsNotNone(row)
        self.assertEqual(row["kind"], "entities")
        self.assertEqual(row["matched_rule"], "heuristic:2")
        self.assertTrue(row["artifact"].startswith("wiki/entities/"))

    def test_procedure_and_question_rules(self):
        wiki = fresh()
        # predicate "runbook" -> heuristic:1 -> procedures
        save_claim(wiki, "deploy api", predicate="runbook",
                   authority={"type": "explicit_project_decision"})
        # code identifier with >=2 claims -> heuristic:3 -> entities
        save_claim(wiki, "auth_gateway", predicate="engine",
                   root_origin="origin-1")
        save_claim(wiki, "auth_gateway", predicate="version",
                   root_origin="origin-2")
        # "open question" in the subject -> heuristic:4 -> questions
        save_claim(wiki, "open questions billing", predicate="status",
                   authority={"type": "explicit_project_decision"})
        t1 = self._row(wiki, "deploy api")
        self.assertEqual((t1["kind"], t1["matched_rule"]),
                         ("procedures", "heuristic:1"))
        t2 = self._row(wiki, "auth_gateway")
        self.assertEqual((t2["kind"], t2["matched_rule"]),
                         ("entities", "heuristic:3"))
        t3 = self._row(wiki, "open questions billing")
        self.assertEqual((t3["kind"], t3["matched_rule"]),
                         ("questions", "heuristic:4"))

    def test_file_rule_beats_heuristic(self):
        wiki = fresh()
        self._rules(wiki, {"rules": [{"kind": "concepts", "regex": "^stc-"}]})
        save_claim(wiki, "stc-99",
                   authority={"type": "explicit_project_decision"})
        row = self._row(wiki, "stc-99")
        self.assertEqual(row["kind"], "concepts")
        self.assertEqual(row["matched_rule"], 0)

    def test_default_concepts_and_catchall(self):
        wiki = fresh()
        # unmatched subject (spaces -> not an identifier) -> default concepts
        save_claim(wiki, "billing overview",
                   authority={"type": "explicit_project_decision"})
        row = self._row(wiki, "billing overview")
        self.assertEqual(row["kind"], "concepts")
        self.assertIsNone(row["matched_rule"])
        # catch-all file rule pins everything to concepts
        self._rules(wiki, {"rules": [{"kind": "concepts", "regex": ".*"}]})
        save_claim(wiki, "stc-77", predicate="runbook",
                   authority={"type": "explicit_project_decision"})
        row = self._row(wiki, "stc-77")
        self.assertEqual(row["kind"], "concepts")
        self.assertEqual(row["matched_rule"], 0)

    def test_empty_rules_file_activates_heuristics(self):
        wiki = fresh()
        self._rules(wiki, {"rules": []})
        save_claim(wiki, "stc-5",
                   authority={"type": "explicit_project_decision"})
        row = self._row(wiki, "stc-5")
        self.assertEqual((row["kind"], row["matched_rule"]),
                         ("entities", "heuristic:2"))

    def test_unmatched_file_rule_falls_through_to_heuristic(self):
        # a file rule that doesn't match this subject must NOT disable
        # heuristics — file rules extend, they don't replace
        wiki = fresh()
        self._rules(wiki, {"rules": [{"kind": "concepts", "regex": "^stc-"}]})
        save_claim(wiki, "auth_gateway",
                   authority={"type": "explicit_project_decision"})
        save_claim(wiki, "auth_gateway", predicate="owner",
                   authority={"type": "authoritative_source"})
        row = self._row(wiki, "auth_gateway")
        self.assertEqual((row["kind"], row["matched_rule"]),
                         ("entities", "heuristic:3"))



class TestPageLinks(unittest.TestCase):
    def test_bundle_relative_link_appends_dep(self):
        wiki = fresh()
        c1 = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        c2 = save_claim(wiki, "s3", authority={"type": "explicit_project_decision"})
        dep1 = ["%s@1" % c1["id"]]
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep1), dep1,
                              wiki.revision())
        dep2 = ["%s@1" % c2["id"]]
        body = "Stores sit on [the DB](wiki/concepts/db.md)."
        r = synthesize.write_page(wiki, "wiki/entities/s3.md",
                                  page_md(dep2, body=body, type_="entity",
                                          subject="s3"),
                                  dep2, wiki.revision(), allow_new=True)
        self.assertIn("wiki/concepts/db.md", r["deps"])
        self.assertEqual(deps.graph(wiki)["edges"]["wiki/entities/s3.md"],
                         sorted(r["deps"]))

    def test_relative_link_resolves_against_artifact_dir(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep), dep,
                              wiki.revision())
        body = "Back to [db](./db.md)."
        r = synthesize.write_page(wiki, "wiki/concepts/s3.md",
                                  page_md(dep, body=body, subject="s3"), dep,
                                  wiki.revision(), allow_new=True)
        self.assertIn("wiki/concepts/db.md", r["deps"])

    def test_unresolved_bundle_link_rejected(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        body = "See [nope](../entities/nope.md)."
        with self.assertRaises(TxnError):
            synthesize.write_page(wiki, "wiki/concepts/db.md",
                                  page_md(dep, body=body), dep,
                                  wiki.revision())

    def test_external_and_non_md_links_ignored(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        body = "[site](https://x.example/a.md) [img](./pic.png) [out](../../outside/a.md)"
        r = synthesize.write_page(wiki, "wiki/concepts/db.md",
                                  page_md(dep, body=body), dep,
                                  wiki.revision())
        self.assertEqual(r["deps"], dep)

    def test_reserved_log_and_review_rejected(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        for artifact in ("wiki/log.md", "wiki/review.md"):
            with self.assertRaises(TxnError):
                synthesize.write_page(wiki, artifact, page_md(dep), dep,
                                      wiki.revision())

    def test_owner_backfilled_from_root_origin(self):
        wiki = fresh()
        c = save_claim(wiki, "db", root_origin="platform-adr",
                       authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep), dep,
                              wiki.revision())
        with open(wiki.p("wiki/concepts/db.md")) as f:
            front, _ = synthesize._split_front(f.read())
        self.assertEqual(front.get("owner"), "platform-adr")

    def test_explicit_owner_preserved(self):
        wiki = fresh()
        c = save_claim(wiki, "db", root_origin="platform-adr",
                       authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        md = page_md(dep).replace("title: DB engine",
                                  "title: DB engine\nowner: data-team")
        synthesize.write_page(wiki, "wiki/concepts/db.md", md, dep,
                              wiki.revision())
        with open(wiki.p("wiki/concepts/db.md")) as f:
            front, _ = synthesize._split_front(f.read())
        self.assertEqual(front.get("owner"), "data-team")

    def test_body_change_stales_linking_pages(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        c2 = save_claim(wiki, "s3", authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep), dep,
                              wiki.revision())
        dep2 = ["%s@1" % c2["id"]]
        body = "Stores sit on [db](../concepts/db.md)."
        synthesize.write_page(wiki, "wiki/entities/s3.md",
                              page_md(dep2, body=body, type_="entity",
                                      subject="s3"),
                              dep2, wiki.revision(), allow_new=True)
        # rewrite db.md with a changed body -> s3.md goes stale
        synthesize.write_page(wiki, "wiki/concepts/db.md",
                              page_md(dep, body="CHANGED BODY"), dep,
                              wiki.revision())
        self.assertIn("wiki/entities/s3.md", deps.stale_artifacts(wiki))
        self.assertNotIn("wiki/concepts/db.md", deps.stale_artifacts(wiki))

    def test_identical_body_rewrite_does_not_stale_linkers(self):
        wiki = fresh()
        c = save_claim(wiki, "db", authority={"type": "explicit_project_decision"})
        c2 = save_claim(wiki, "s3", authority={"type": "explicit_project_decision"})
        dep = ["%s@1" % c["id"]]
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep), dep,
                              wiki.revision())
        dep2 = ["%s@1" % c2["id"]]
        body = "Stores sit on [db](../concepts/db.md)."
        synthesize.write_page(wiki, "wiki/entities/s3.md",
                              page_md(dep2, body=body, type_="entity",
                                      subject="s3"),
                              dep2, wiki.revision(), allow_new=True)
        # stale-clear rewrite with the same body leaves s3.md fresh
        synthesize.write_page(wiki, "wiki/concepts/db.md", page_md(dep), dep,
                              wiki.revision())
        self.assertNotIn("wiki/entities/s3.md", deps.stale_artifacts(wiki))


class TestRelatedTargets(unittest.TestCase):
    def test_related_lists_same_prefix_siblings(self):
        wiki = fresh()
        txn = Transaction(wiki, "t")
        topics.merge_rows(txn, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3"},
            {"subject": "lake.gcs", "kind": "entity", "title": "GCS"},
            {"subject": "wh.bq", "kind": "entity", "title": "BigQuery"},
        ], {"source_id": "src_a", "version": 1})
        txn.commit(wiki.revision())
        save_claim(wiki, "lake.s3", authority={"type": "explicit_project_decision"})
        targets = synthesize.page_targets(wiki)["targets"]
        t = [x for x in targets if x["subject"] == "lake.s3"][0]
        self.assertEqual(t["related"], ["lake.gcs"])
if __name__ == "__main__":
    unittest.main()
