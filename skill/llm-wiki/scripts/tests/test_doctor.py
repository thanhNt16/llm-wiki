import json, os, sys, tempfile, time, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, deps, doctor, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "doctor-test")
    return wiki

def write_raw_page(wiki, rel, subject, extra_front=None):
    """Drop a wiki/<kind>/<slug>.md page on disk without write_page's
    slug/dep validation — doctor fixtures need shapes write_page refuses."""
    path = wiki.p(rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    lines = ["---", "type: concept", "subject: %s" % subject,
             "title: %s" % subject, "created: 2026-09-15T00:00:00Z",
             "updated: 2026-09-15", "deps: []", "stale: false"]
    for k, v in (extra_front or {}).items():
        lines.append("%s: %s" % (k, v))
    lines += ["---", "", "Body text for %s." % subject, ""]
    with open(path, "w") as f:
        f.write("\n".join(lines))


def set_page_edges(wiki, mapping):
    """Register page->dep edges exactly (replace semantics)."""
    txn = Transaction(wiki, "page-edges")
    for rel, ds in mapping.items():
        deps.replace(txn, rel, ds)
    txn.commit(wiki.revision())



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
        sev = {f["code"]: f["severity"] for f in out["findings"]}
        self.assertEqual(sev.get("missing_scope"), "info", out["findings"])
        self.assertEqual(sev.get("missing_effective_date"), "info", out["findings"])

    def test_detects_staging_leftover(self):
        wiki = fresh()
        os.makedirs(wiki.p(".state/staging", "run_01JLEFTOVERLEFTOVERLEFTOVER"),
                    exist_ok=True)
        out = doctor.run(wiki)
        sev = {f["code"]: f["severity"] for f in out["findings"]}
        self.assertEqual(sev.get("staging_leftover"), "info", out["findings"])

    def test_unextracted_vs_orphan_source_split(self):
        # never-cited source -> unextracted_source; cited but covered by no
        # page dep -> orphan_source (both info).
        wiki = fresh()
        c = add_accepted_claim(wiki)  # cites its source; no page deps it
        sid_cited = c["evidence"][0]["source_id"]
        os.makedirs(wiki.p("sources", "source_" + "B" * 26), exist_ok=True)
        out = doctor.run(wiki)
        sev = {}
        for f in out["findings"]:
            if f["code"] in ("unextracted_source", "orphan_source"):
                sev.setdefault(f["code"], []).append((f["severity"], f["detail"]))
        self.assertTrue(any(s == "info" and sid_cited in d
                            for s, d in sev.get("orphan_source", [])),
                        out["findings"])
        self.assertTrue(any(s == "info" and ("B" * 26) in d
                            for s, d in sev.get("unextracted_source", [])),
                        out["findings"])
        # covering the claim with a page dep clears orphan_source
        write_raw_page(wiki, "wiki/concepts/s.md", "s")
        set_page_edges(wiki, {"wiki/concepts/s.md": ["%s@%d" % (c["id"], c["version"])]})
        out = doctor.run(wiki)
        self.assertFalse(any(f["code"] == "orphan_source" and sid_cited in f["detail"]
                             for f in out["findings"]), out["findings"])
        self.assertTrue(any(f["code"] == "unextracted_source"
                            for f in out["findings"]), out["findings"])

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

    def test_detects_orphan_claim(self):
        wiki = fresh()
        c = add_accepted_claim(wiki)
        txn = Transaction(wiki, "strip")
        c["evidence"] = []
        claims.save_claim(txn, c)
        txn.commit(wiki.revision())
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "orphan_claim" for f in out["findings"]))

    def _queue_append(self, wiki, line):
        with open(wiki.p(".state/review-queue.jsonl"), "a") as f:
            f.write(line + "\n")

    def test_detects_malformed_review_lines(self):
        wiki = fresh()
        self._queue_append(wiki, "{not json")
        self._queue_append(wiki, json.dumps({"id": "review_x", "status": "open"}))
        out = doctor.run(wiki)
        codes = [f["code"] for f in out["findings"]]
        self.assertEqual(codes.count("malformed_review_item"), 2)
        self.assertFalse(out["ok"])

    def test_detects_missing_affected_id(self):
        wiki = fresh()
        self._queue_append(wiki, json.dumps({
            "id": "review_x", "created_at": "2026-10-01T00:00:00Z",
            "kind": "authority_conflict", "status": "open",
            "problem": "p", "affected": ["claim_NOPE"], "risk": "high"}))
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "broken_reference"
                            and "claim_NOPE" in f["detail"]
                            for f in out["findings"]))

    def test_detects_deferred_aging(self):
        wiki = fresh()
        old = time.strftime("%Y-%m-%dT%H:%M:%SZ",
                            time.gmtime(time.time() - 8 * 86400))
        self._queue_append(wiki, json.dumps({
            "id": "review_x", "created_at": old, "kind": "authority_conflict",
            "status": "deferred", "deferred_at": old, "problem": "p",
            "affected": [], "risk": "high"}))
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "deferred_aging"
                            for f in out["findings"]))

    def test_detects_unregistered_page_and_journal(self):
        wiki = fresh()
        os.makedirs(wiki.p("wiki/questions"), exist_ok=True)
        with open(wiki.p("wiki/questions/x.md"), "w") as f:
            f.write("---\ntype: question\ndeps: []\n---\n\nbody\n")
        with open(wiki.p(".state/journal.json"), "w") as f:
            f.write("{}")
        out = doctor.run(wiki)
        codes = [f["code"] for f in out["findings"]]
        self.assertIn("unregistered_page", codes)
        self.assertIn("transaction_journal_leftover", codes)
        self.assertFalse(out["ok"])

    def test_corrupt_decision_json_flagged_not_crash(self):
        wiki = fresh()
        with open(wiki.p("decisions/decision_BAD.json"), "w") as f:
            f.write("{not json")
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "schema_invalid"
                            and "decisions/" in f["detail"]
                            for f in out["findings"]))

    def test_corrupt_claim_json_does_not_crash_later_checks(self):
        # a corrupt claims/*.json is reported once; every later check must
        # still run instead of crashing inside claims.list_claims
        wiki = fresh()
        add_accepted_claim(wiki)
        with open(wiki.p("claims/claim_BROKEN.json"), "w") as f:
            f.write("{not json")
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "schema_invalid"
                            and "claims/" in f["detail"]
                            for f in out["findings"]))
        # warn-level checks still ran past the corrupt file (missing_scope)
        self.assertTrue(any(f["code"] == "missing_scope"
                            for f in out["findings"]))

    def test_detects_stale_decision(self):
        wiki = fresh()
        c = add_accepted_claim(wiki)
        txn = Transaction(wiki, "mkdec")
        claims.save_decision(txn, {
            "id": "decision_" + "D" * 26, "title": "Use plan", "status": "accepted",
            "decided_on": "2026-01-10", "recorded_at": "2026-09-15T00:00:00Z",
            "version": 1, "claims": [c["id"]], "evidence": []})
        txn.commit(wiki.revision())
        # the backing claim gets superseded — decision is now stale
        path = wiki.p("claims/%s.json" % c["id"])
        with open(path) as f:
            data = json.load(f)
        data["status"] = "superseded"
        data["superseded_by"] = "claim_" + "E" * 26
        with open(path, "w") as f:
            json.dump(data, f)
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "stale_decision"
                            and "decision_" + "D" * 26 in f["detail"]
                            for f in out["findings"]))

    def test_detects_possible_duplicate(self):
        wiki = fresh()
        c1 = add_accepted_claim(wiki, "cache", "ttl", 60)
        c2 = add_accepted_claim(wiki, "dup", "p", 1)
        path = wiki.p("claims/%s.json" % c2["id"])
        with open(path) as f:
            data = json.load(f)
        data["subject"] = "cache"
        data["predicate"] = "ttl"
        data["value"] = 60
        with open(path, "w") as f:
            json.dump(data, f)
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "possible_duplicate"
                            and c1["id"] in f["detail"] and c2["id"] in f["detail"]
                            for f in out["findings"]))

    def test_detects_stale_candidate(self):
        wiki = fresh()
        c = add_accepted_claim(wiki, "backlog", "item", 1,
                               authority="agent_inference")
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "stale_candidate"
                            and c["id"] in f["detail"]
                            for f in out["findings"]))

    def test_stale_candidate_suppressed_when_under_review(self):
        from wikicore import review
        wiki = fresh()
        c = add_accepted_claim(wiki, "backlog", "item", 1,
                               authority="agent_inference")
        txn = Transaction(wiki, "mkrev")
        review.add_item(txn, "possible_contradiction", "unclear",
                        [], [c["id"]], "low")
        txn.commit(wiki.revision())
        out = doctor.run(wiki)
        self.assertFalse(any(f["code"] == "stale_candidate"
                             and c["id"] in f["detail"]
                             for f in out["findings"]))

    def test_detects_orphan_decision(self):
        wiki = fresh()
        txn = Transaction(wiki, "mkdec")
        claims.save_decision(txn, {
            "id": "decision_" + "D" * 26, "title": "Bare call", "status": "accepted",
            "decided_on": "2026-01-10", "recorded_at": "2026-09-15T00:00:00Z",
            "version": 1, "claims": [], "evidence": []})
        txn.commit(wiki.revision())
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "orphan_decision"
                            for f in out["findings"]))

    def test_detects_unexpected_lock_file(self):
        wiki = fresh()
        with open(wiki.p(".state/locks/weird.lock"), "w") as f:
            f.write("")
        out = doctor.run(wiki)
        self.assertTrue(any(f["code"] == "stale_lock"
                            for f in out["findings"]))

    def test_detects_missing_artifact_and_unresolved_dep(self):
        wiki = fresh()
        c = add_accepted_claim(wiki)
        txn = Transaction(wiki, "mkedge")
        deps.register(txn, "views/gone.txt",
                      ["%s@%d" % (c["id"], c["version"]), "claim_NOPE@1"])
        txn.commit(wiki.revision())
        out = doctor.run(wiki)
        codes = {f["code"] for f in out["findings"]}
        self.assertIn("missing_artifact", codes)
        self.assertTrue(any(f["code"] == "unresolved_dependency"
                            and "claim_NOPE" in f["detail"]
                            for f in out["findings"]))


def _write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


PAGE_TMPL = """---
type: concept
subject: %s
title: %s
created: 2026-09-15T00:00:00Z
---

# %s

See [[%s]].
Related: [[wiki:concepts/%s]].
"""


class TopologyFindingsTest(unittest.TestCase):
    def setUp(self):
        self.wiki = fresh()
        self.c = add_accepted_claim(self.wiki, "subject alpha", "has", 1)
        # a healthy section: index lists the pages, pages cross-link via
        # [[wiki:]] (v2 graph semantics: inbound AND outbound edges exist)
        _write(self.wiki.p("wiki", "concepts", "subject-alpha.md"),
               PAGE_TMPL % ("subject alpha", "Subject Alpha", "Subject Alpha",
                            self.c["id"], "related-note"))
        _write(self.wiki.p("wiki", "concepts", "related-note.md"),
               PAGE_TMPL % ("related note", "Related Note", "Related Note",
                            self.c["id"], "subject-alpha"))
        _write(self.wiki.p("wiki", "concepts", "index.md"),
               "# Index\n\n| Page | Title |\n|---|---|\n"
               "| [subject-alpha](subject-alpha.md) | Subject Alpha |\n"
               "| [related-note](related-note.md) | Related Note |\n")
        _write(self.wiki.p("wiki", "overview.md"),
               "# Overview\n\n- [concepts](concepts/index.md) (2)\n")

    def _codes(self):
        out = doctor.run(self.wiki)
        return [f["code"] for f in out["findings"]], out

    def test_healthy_section_has_no_topology_findings(self):
        codes, out = self._codes()
        self.assertNotIn("orphan_page", codes)
        self.assertNotIn("dead_end_page", codes)
        self.assertNotIn("unlisted_page", codes)
        self.assertNotIn("section_index_unlinked", codes)
        self.assertTrue(out["ok"])  # warnings never fail doctor

    def test_unlisted_page_flagged(self):
        _write(self.wiki.p("wiki", "concepts", "stray-page.md"),
               PAGE_TMPL % ("stray page", "Stray", "Stray",
                            self.c["id"], "subject-alpha"))
        codes, _ = self._codes()
        self.assertIn("unlisted_page", codes)
        detail = [f for f in doctor.run(self.wiki)["findings"]
                  if f["code"] == "unlisted_page"][0]["detail"]
        self.assertIn("stray-page.md", detail)

    def test_dead_end_page_flagged(self):
        # v2 semantics: no [[wiki:]] links -> dead end even though the page
        # still cites its claims
        _write(self.wiki.p("wiki", "concepts", "subject-alpha.md"),
               "---\ntype: concept\nsubject: subject alpha\n"
               "title: Subject Alpha\ncreated: 2026-09-15T00:00:00Z\n---\n\n"
               "# Subject Alpha\n\nSee [[%s]].\n" % self.c["id"])
        codes, _ = self._codes()
        self.assertIn("dead_end_page", codes)
        detail = [f for f in doctor.run(self.wiki)["findings"]
                  if f["code"] == "dead_end_page"
                  and "subject-alpha.md" in f["detail"]][0]["detail"]
        self.assertIn("cites claims", detail)

    def test_dead_end_page_no_links_at_all(self):
        _write(self.wiki.p("wiki", "concepts", "subject-alpha.md"),
               "---\ntype: concept\nsubject: subject alpha\n"
               "title: Subject Alpha\ncreated: 2026-09-15T00:00:00Z\n---\n\n"
               "# Subject Alpha\n\nNo links at all here.\n")
        codes, _ = self._codes()
        self.assertIn("dead_end_page", codes)
        detail = [f for f in doctor.run(self.wiki)["findings"]
                  if f["code"] == "dead_end_page"
                  and "subject-alpha.md" in f["detail"]][0]["detail"]
        self.assertNotIn("cites claims", detail)

    def test_section_index_unlinked_flagged(self):
        _write(self.wiki.p("wiki", "overview.md"), "# Overview\n\n- nothing\n")
        codes, _ = self._codes()
        self.assertIn("section_index_unlinked", codes)

    def test_indexes_and_overview_exempt(self):
        codes, _ = self._codes()
        # index.md and overview.md must never be flagged as orphan/dead-end
        details = " ".join(f["detail"] for f in doctor.run(self.wiki)["findings"])
        self.assertNotIn("index.md is not listed", details.replace("concepts/index", "X"))


class GraphLintTest(unittest.TestCase):
    def setUp(self):
        self.wiki = fresh()
        self.c = add_accepted_claim(self.wiki, "alpha", "has", 1)

    def _page(self, rel, subject, title, body):
        os.makedirs(os.path.dirname(self.wiki.p(rel)), exist_ok=True)
        with open(self.wiki.p(rel), "w") as f:
            f.write("---\ntype: concept\nsubject: %s\ntitle: %s\n"
                    "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
                    % (subject, title, title, body))
        # register deps so verify stays coherent
        txn = Transaction(self.wiki, "test-page")
        with open(self.wiki.p(rel)) as fh:
            txn.stage_write(rel, fh.read())
        txn.commit(self.wiki.revision())

    def test_walled_garden_and_orphan(self):
        # {a,b,c}: a 3-page component linked only among itself, with no
        # edge to the rest of the wiki -> walled garden. {d,e,f,g}: the
        # larger main component (never a garden). h: no edges at all ->
        # orphan.
        self._page("wiki/concepts/a.md", "a subject", "A",
                   "[[wiki:concepts/b]]")
        self._page("wiki/concepts/b.md", "b subject", "B",
                   "[[wiki:concepts/c]]")
        self._page("wiki/concepts/c.md", "c subject", "C",
                   "[[wiki:concepts/a]]")
        self._page("wiki/concepts/d.md", "d subject", "D",
                   "[[wiki:concepts/e]]")
        self._page("wiki/concepts/e.md", "e subject", "E",
                   "[[wiki:concepts/f]]")
        self._page("wiki/concepts/f.md", "f subject", "F",
                   "[[wiki:concepts/d]] [[wiki:concepts/g]]")
        self._page("wiki/concepts/g.md", "g subject", "G",
                   "[[wiki:concepts/d]]")
        self._page("wiki/concepts/h.md", "h subject", "H", "isolated")
        out = doctor.run(self.wiki)
        gardens = [f["detail"] for f in out["findings"]
                   if f["code"] == "walled_garden"]
        self.assertEqual(len(gardens), 1, out["findings"])
        for m in ("a.md", "b.md", "c.md"):
            self.assertIn(m, gardens[0])
        self.assertNotIn("d.md", gardens[0])
        orphans = [f["detail"] for f in out["findings"]
                   if f["code"] == "orphan_page"]
        self.assertEqual(len(orphans), 1, out["findings"])
        self.assertIn("h.md", orphans[0])

    def test_stale_ttl_and_near_dup(self):
        self._page("wiki/concepts/old.md", "old subject", "Old", "[[c_x]]")
        with open(self.wiki.p("wiki/concepts/old.md")) as f:
            text = f.read()
        with open(self.wiki.p("wiki/concepts/old.md"), "w") as f:
            f.write(text.replace("created:", "stale_after: 2026-01-01\ncreated:"))
        self._page("wiki/concepts/alpha.md", "alpha topic", "Alpha Topic", "[[c_x]]")
        self._page("wiki/concepts/alpha-copy.md", "alpha topics", "Alpha Topics", "[[c_x]]")
        codes = [f["code"] for f in doctor.run(self.wiki)["findings"]]
        self.assertIn("stale_page_ttl", codes)
        self.assertIn("near_duplicate_subject", codes)

    def test_malformed_frontmatter_is_error_not_crash(self):
        with open(self.wiki.p("wiki/concepts/bad.md"), "w") as f:
            f.write("---\nnot: yaml: at: all\n|\nunclosed\n")
        out = doctor.run(self.wiki)
        f = [x for x in out["findings"] if x["code"] == "malformed_frontmatter"]
        self.assertTrue(f)
        self.assertEqual(f[0]["severity"], "error")


class TestPageGraphFindings(unittest.TestCase):
    """Page-graph diagnostics over [[wiki:]] body links + registered page
    deps (unified two-tier lint semantics)."""

    def _flagged(self, out, code):
        return [f["detail"] for f in out["findings"] if f["code"] == code]

    def test_orphan_and_dead_end_pages(self):
        wiki = fresh()
        write_raw_page(wiki, "wiki/concepts/a-page.md", "alpha")
        write_raw_page(wiki, "wiki/concepts/b-page.md", "beta")
        write_raw_page(wiki, "wiki/concepts/c-page.md", "gamma")
        set_page_edges(wiki, {
            "wiki/concepts/a-page.md": ["wiki/concepts/b-page.md"],
            "wiki/concepts/b-page.md": [],
            "wiki/concepts/c-page.md": [],
        })
        out = doctor.run(wiki)
        orphans = self._flagged(out, "orphan_page")
        dead_ends = self._flagged(out, "dead_end_page")
        # a: outbound only -> orphan; b: inbound only -> dead-end; c: neither -> both
        self.assertTrue(any("a-page.md" in d for d in orphans))
        self.assertTrue(any("c-page.md" in d for d in orphans))
        self.assertFalse(any("b-page.md" in d for d in orphans))
        self.assertTrue(any("b-page.md" in d for d in dead_ends))
        self.assertTrue(any("c-page.md" in d for d in dead_ends))
        self.assertFalse(any("a-page.md" in d for d in dead_ends))
        # page-graph findings are warnings, never errors
        for f in out["findings"]:
            if f["code"] in ("orphan_page", "dead_end_page"):
                self.assertEqual(f["severity"], "warn")

    def test_walled_garden_min_component_three(self):
        wiki = fresh()
        # main linked body (4 pages, fully connected) + detached 2-cluster
        write_raw_page(wiki, "wiki/concepts/m1-page.md", "main one")
        write_raw_page(wiki, "wiki/concepts/m2-page.md", "main two")
        write_raw_page(wiki, "wiki/concepts/m3-page.md", "main three")
        write_raw_page(wiki, "wiki/concepts/m4-page.md", "main four")
        write_raw_page(wiki, "wiki/concepts/d-page.md", "delta")
        write_raw_page(wiki, "wiki/concepts/e-page.md", "epsilon")
        write_raw_page(wiki, "wiki/concepts/g-page.md", "other")
        main_edges = ["wiki/concepts/m%d-page.md" % i for i in (1, 2, 3, 4)]
        set_page_edges(wiki, {
            "wiki/concepts/m1-page.md": [p for p in main_edges
                                         if p != "wiki/concepts/m1-page.md"],
            "wiki/concepts/m2-page.md": [],
            "wiki/concepts/m3-page.md": [],
            "wiki/concepts/m4-page.md": [],
            "wiki/concepts/d-page.md": ["wiki/concepts/e-page.md"],
            "wiki/concepts/e-page.md": ["wiki/concepts/d-page.md"],
            "wiki/concepts/g-page.md": [],
        })
        out = doctor.run(wiki)
        self.assertEqual(self._flagged(out, "walled_garden"), [])
        # grow the detached component to 3 -> walled; main body never flagged
        write_raw_page(wiki, "wiki/concepts/f-page.md", "zeta")
        set_page_edges(wiki, {
            "wiki/concepts/d-page.md": ["wiki/concepts/e-page.md",
                                        "wiki/concepts/f-page.md"],
            "wiki/concepts/f-page.md": ["wiki/concepts/d-page.md"],
        })
        out = doctor.run(wiki)
        gardens = self._flagged(out, "walled_garden")
        self.assertEqual(len(gardens), 1, out["findings"])
        self.assertIn("d-page.md", gardens[0])
        self.assertIn("e-page.md", gardens[0])
        self.assertIn("f-page.md", gardens[0])
        self.assertNotIn("m1-page.md", gardens[0])

    def test_malformed_frontmatter_does_not_crash(self):
        wiki = fresh()
        # yamlite.loads raises TxnError on a frontmatter line without a colon
        path = wiki.p("wiki/concepts/broken.md")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write("---\nthis line has no colon\n---\n\nBody.\n")
        out = doctor.run(wiki)  # must not raise; page skipped for subject/TTL
        self.assertIsInstance(out["findings"], list)
        codes = {f["code"] for f in out["findings"]}
        self.assertNotIn("near_duplicate_subject", codes)
        self.assertNotIn("stale_page_ttl", codes)

    def test_near_duplicate_subject(self):
        wiki = fresh()
        write_raw_page(wiki, "wiki/concepts/x-page.md",
                       "order attribution window")
        write_raw_page(wiki, "wiki/concepts/y-page.md",
                       "order attribution windows")
        write_raw_page(wiki, "wiki/concepts/z-page.md",
                       "completely different subject")
        set_page_edges(wiki, {
            "wiki/concepts/x-page.md": [],
            "wiki/concepts/y-page.md": [],
            "wiki/concepts/z-page.md": [],
        })
        out = doctor.run(wiki)
        dupes = self._flagged(out, "near_duplicate_subject")
        self.assertEqual(len(dupes), 1, out["findings"])
        self.assertIn("x-page.md", dupes[0])
        self.assertIn("y-page.md", dupes[0])

    def test_near_duplicate_subject_suppressed_by_link(self):
        wiki = fresh()
        write_raw_page(wiki, "wiki/concepts/x-page.md",
                       "order attribution window")
        write_raw_page(wiki, "wiki/concepts/y-page.md",
                       "order attribution windows")
        set_page_edges(wiki, {
            "wiki/concepts/x-page.md": ["wiki/concepts/y-page.md"],
            "wiki/concepts/y-page.md": [],
        })
        out = doctor.run(wiki)
        self.assertEqual(self._flagged(out, "near_duplicate_subject"), [])

    def test_stale_page_ttl(self):
        wiki = fresh()
        write_raw_page(wiki, "wiki/concepts/old-page.md", "alpha",
                       extra_front={"stale_after": "2020-01-01"})
        write_raw_page(wiki, "wiki/concepts/fresh-page.md", "beta",
                       extra_front={"stale_after": "2999-01-01"})
        write_raw_page(wiki, "wiki/concepts/ttl-free.md", "gamma")
        write_raw_page(wiki, "wiki/concepts/bad-date.md", "delta",
                       extra_front={"stale_after": "not-a-date"})
        set_page_edges(wiki, {
            "wiki/concepts/old-page.md": [],
            "wiki/concepts/fresh-page.md": [],
            "wiki/concepts/ttl-free.md": [],
            "wiki/concepts/bad-date.md": [],
        })
        out = doctor.run(wiki)
        ttl = self._flagged(out, "stale_page_ttl")
        self.assertEqual(len(ttl), 1, out["findings"])
        self.assertIn("old-page.md", ttl[0])



class RepairPlanTest(unittest.TestCase):
    """repair_plan: machine-readable per-finding editorial actions
    ({"code", "path", "action"}); engine suggests, never writes prose."""

    def test_repair_plan_lists_editorial_actions(self):
        wiki = fresh()
        add_accepted_claim(wiki, "alpha", "has", 1)
        # a lone page with no wiki links -> dead_end (single page can't be
        # orphan) + unlisted in its (absent) section index
        write_raw_page(wiki, "wiki/concepts/lone.md", "lone")
        plan = doctor.run(wiki)["repair_plan"]
        self.assertTrue(plan)
        codes = {p["code"] for p in plan}
        self.assertTrue(any(p["code"] in ("orphan_page", "dead_end_page")
                            for p in plan), plan)
        self.assertIn("unlisted_page", codes)
        for p in plan:
            self.assertEqual({"code", "path", "action"}, set(p))
            self.assertTrue(p["path"].startswith("wiki/"), p)
            self.assertIn("[[wiki:", p["action"], p)
        lone = [p for p in plan if p["path"] == "wiki/concepts/lone.md"]
        self.assertTrue(any("[[wiki:concepts/lone]]" in p["action"]
                            for p in lone), lone)

    def test_repair_plan_empty_on_clean_indexed_graph(self):
        wiki = fresh()
        add_accepted_claim(wiki, "alpha", "has", 1)
        # healthy two-page linked section, indexed and overview-linked
        _write(wiki.p("wiki", "concepts", "a-page.md"),
               PAGE_TMPL % ("a subject", "A Page", "A Page",
                            "claim_x", "b-page"))
        _write(wiki.p("wiki", "concepts", "b-page.md"),
               PAGE_TMPL % ("b subject", "B Page", "B Page",
                            "claim_x", "a-page"))
        _write(wiki.p("wiki", "concepts", "index.md"),
               "# Index\n\n| Page | Title |\n|---|---|\n"
               "| [a-page](a-page.md) | A Page |\n"
               "| [b-page](b-page.md) | B Page |\n")
        _write(wiki.p("wiki", "overview.md"),
               "# Overview\n\n- [concepts](concepts/index.md) (2)\n")
        plan = doctor.run(wiki)["repair_plan"]
        self.assertEqual(plan, [], plan)


class AutoDecisionStubFindingTest(unittest.TestCase):
    """auto-materialized accepted decisions surface as an info finding —
    reviewable, never an error."""

    def _stub_findings(self, out):
        return [f for f in out["findings"] if f["code"] == "auto_decision_stub"]

    def test_absent_without_stubs(self):
        wiki = fresh()
        out = doctor.run(wiki)
        self.assertEqual(self._stub_findings(out), [])

    def test_info_finding_counts_stubs(self):
        wiki = fresh()
        add_accepted_claim(wiki, "alpha", "has", 1)
        add_accepted_claim(wiki, "beta", "has", 2)
        self.assertEqual(len(claims.list_decisions(wiki)), 2)
        out = doctor.run(wiki)
        stub = self._stub_findings(out)
        self.assertEqual(len(stub), 1)
        self.assertEqual(stub[0]["severity"], "info")
        self.assertIn("2 auto-materialized decision stub", stub[0]["detail"])
        # info severity never fails the run
        self.assertTrue(out["ok"], out["findings"])

    def test_retracted_stub_not_counted(self):
        wiki = fresh()
        add_accepted_claim(wiki, "alpha", "has", 1)
        d = claims.list_decisions(wiki)[0]
        d["status"] = "retracted"
        txn = Transaction(wiki, "retract")
        claims.save_decision(txn, d)
        txn.commit(wiki.revision())
        out = doctor.run(wiki)
        self.assertEqual(self._stub_findings(out), [])


if __name__ == "__main__":
    unittest.main()
