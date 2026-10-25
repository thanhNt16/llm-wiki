import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import query
from wikicore.store import Wiki, init_wiki


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "bm25-test")
    return wiki


def put_claim_json(wiki, cid, subject, predicate, value, status="accepted",
                   **extra):
    import json
    claim = {"id": cid, "subject": subject, "predicate": predicate,
             "value": value, "scope": {}, "valid_from": None,
             "valid_to": None, "recorded_at": "2026-09-15T00:00:00Z",
             "status": status, "proposed_by": "agent",
             "authority": {"type": "explicit_project_decision"},
             "evidence": [{"source_id": "src_x", "source_version": 1,
                           "locator": {"type": "text", "value": "x"}}],
             "supersedes": [], "version": 1}
    claim.update(extra)
    os.makedirs(wiki.p("claims"), exist_ok=True)
    with open(wiki.p("claims/%s.json" % cid), "w") as f:
        json.dump(claim, f)


def put_page(wiki, rel, title, deps=None, subject=None):
    """Write a wiki/<kind>/<slug>.md page with minimal frontmatter."""
    path = wiki.p(rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    front = ["---", "type: %s" % rel.split("/")[1].rstrip("s")]
    if subject:
        front.append("subject: %s" % subject)
    front.append("title: %s" % title)
    front.append("deps:")
    for d in deps or []:
        front.append("  - %s" % d)
    front.append("stale: false")
    front.append("---")
    with open(path, "w") as f:
        f.write("\n".join(front) + "\n\n# %s\n\nbody\n" % title)


class TestPredicateIDF(unittest.TestCase):
    def test_rare_predicate_outranks_corpus_common_status(self):
        # F2: `status` appears on nearly every subject, so its IDF is low;
        # `root_cause` is rare, so a question about the root cause must
        # surface the root_cause claim ahead of the same subject's status.
        wiki = fresh()
        put_claim_json(wiki, "claim_wx_rc", "widget-x", "root_cause",
                       "missing default in form init")
        put_claim_json(wiki, "claim_wx_st", "widget-x", "status", "open")
        for i, subj in enumerate(("alpha", "beta", "gamma", "delta",
                                  "epsilon", "zeta")):
            put_claim_json(wiki, "claim_st_%d" % i, subj, "status",
                           "in progress")
        out = query.prepare(wiki, "what is the status root cause?")
        ids = [m["claim_id"] for m in out["matches"]]
        self.assertIn("claim_wx_rc", ids)
        self.assertIn("claim_wx_st", ids)
        self.assertLess(ids.index("claim_wx_rc"), ids.index("claim_wx_st"))
        rc = out["matches"][ids.index("claim_wx_rc")]
        st = out["matches"][ids.index("claim_wx_st")]
        self.assertGreater(rc["score_breakdown"]["predicate"],
                           st["score_breakdown"]["predicate"])


class TestSlugAlias(unittest.TestCase):
    def _wiki(self):
        wiki = fresh()
        put_claim_json(wiki, "claim_108", "stc-108", "resolution",
                       "fixed in build 12")
        put_claim_json(wiki, "claim_unrel", "inventory-sync", "status",
                       "green")
        return wiki

    def test_hyphenated_mention_matches_subject(self):
        wiki = self._wiki()
        out = query.prepare(wiki, "how is stc-108 doing?")
        self.assertEqual(out["matches"][0]["claim_id"], "claim_108")

    def test_spaced_mention_matches_subject(self):
        wiki = self._wiki()
        out = query.prepare(wiki, "how is stc 108 doing?")
        self.assertEqual(out["matches"][0]["claim_id"], "claim_108")

    def test_joined_mention_matches_subject(self):
        wiki = self._wiki()
        out = query.prepare(wiki, "how is stc108 doing?")
        self.assertEqual(out["matches"][0]["claim_id"], "claim_108")


class TestTitleAlias(unittest.TestCase):
    def test_question_phrased_by_page_title_reaches_dep_subjects(self):
        # F1: the question never names the subject; a page titled by the
        # paraphrase expands the dep claims' subjects into the subject
        # field query.
        wiki = fresh()
        put_claim_json(wiki, "claim_bridge", "stc-108", "runbook",
                       "apply the documented workaround")
        put_claim_json(wiki, "claim_noise", "inventory-sync", "runbook",
                       "apply the documented workaround")
        put_page(wiki, "wiki/procedures/po-title.md",
                 "Purchase order title blank defect",
                 deps=["claim_bridge@1"])
        out = query.prepare(wiki, "purchase order title blank defect?")
        self.assertEqual(out["matches"][0]["subject"], "stc-108")
        self.assertEqual(out["warnings"], [])


class TestDeterminism(unittest.TestCase):
    def test_same_input_same_ranking(self):
        wiki = fresh()
        put_claim_json(wiki, "claim_a", "stc-108", "status", "open")
        put_claim_json(wiki, "claim_b", "stc-88", "status", "closed")
        put_claim_json(wiki, "claim_c", "billing", "cycle", "monthly")
        q = "status of the deployment?"
        out1 = query.prepare(wiki, q)
        out2 = query.prepare(wiki, q)
        self.assertEqual(out1, out2)
        scores = [(m["claim_id"], m["score"]) for m in out1["matches"]]
        self.assertEqual(scores, sorted(scores,
                                        key=lambda kv: (-kv[1], kv[0])))


class TestEdgeCases(unittest.TestCase):
    def test_empty_wiki_returns_no_matches(self):
        wiki = fresh()
        out = query.prepare(wiki, "anything at all?")
        self.assertEqual(out["matches"], [])
        self.assertEqual(out["warnings"], [])

    def test_malformed_claim_fields_do_not_raise(self):
        wiki = fresh()
        put_claim_json(wiki, "claim_bare", None, None, None)
        put_claim_json(wiki, "claim_ok", "billing", "cycle", "monthly")
        out = query.prepare(wiki, "billing cycle?")
        self.assertEqual([m["claim_id"] for m in out["matches"]],
                         ["claim_ok"])

    def test_malformed_page_frontmatter_warns_not_raises(self):
        wiki = fresh()
        put_claim_json(wiki, "claim_ok", "billing", "cycle", "monthly")
        path = wiki.p("wiki/concepts/broken.md")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write("---\n- not-a-mapping\n---\nbody\n")
        out = query.prepare(wiki, "billing cycle?")
        self.assertTrue(out["warnings"])


class TestAsOfOnBM25(unittest.TestCase):
    def test_as_of_filtering_still_applies(self):
        wiki = fresh()
        put_claim_json(wiki, "claim_old", "deploy", "freeze", "weekly",
                       status="superseded", superseded_by="claim_new",
                       updated_at="2026-10-01T00:00:00Z")
        put_claim_json(wiki, "claim_new", "deploy", "freeze", "daily")
        current = query.prepare(wiki, "deploy freeze?")
        self.assertNotIn("claim_old",
                         [m["claim_id"] for m in current["matches"]])
        past = query.prepare(wiki, "deploy freeze?", as_of="2026-09-15")
        self.assertIn("claim_old",
                      [m["claim_id"] for m in past["matches"]])


class TestScoreFloor(unittest.TestCase):
    def test_relative_floor_trims_weak_tail(self):
        wiki = fresh()
        # strong unique match
        put_claim_json(wiki, "claim_strong", "order service", "database",
                       "postgres")
        # weak tail: shares only a common token with the question
        put_claim_json(wiki, "claim_weak", "unrelated widget", "database",
                       "sqlite")
        out = query.prepare(wiki, "postgres database?")
        ids = [m["claim_id"] for m in out["matches"]]
        self.assertIn("claim_strong", ids)
        self.assertNotIn("claim_weak", ids)

    def test_floor_never_empties_top_match(self):
        wiki = fresh()
        put_claim_json(wiki, "claim_only", "solo", "x", "y")
        out = query.prepare(wiki, "solo x?")
        self.assertEqual([m["claim_id"] for m in out["matches"]],
                         ["claim_only"])


if __name__ == "__main__":
    unittest.main()
