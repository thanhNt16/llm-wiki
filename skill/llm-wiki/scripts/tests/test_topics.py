import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, sources, topics
from wikicore import compile as wc_compile
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "topics-test")
    return wiki


def _commit(wiki, fn):
    txn = Transaction(wiki, "test")
    fn(txn)
    return txn.commit(wiki.revision())


class TestTopicRows(unittest.TestCase):
    def test_parse_rows_validates(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "topics.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"lake.s3","kind":"entity","title":"S3"}\n')
            f.write('{"subject":"lake.dl","kind":"concepts","title":"Data lake"}\n')
        rows, kinds = topics.parse_topics_file(p)
        self.assertEqual(rows[0]["kind"], "entity")
        self.assertEqual(rows[1]["kind"], "concept")  # plural normalized

    def test_parse_rows_rejects_bad_kind(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "t.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a.b","kind":"bogus","title":"T"}\n')
        with self.assertRaises(TxnError):
            topics.parse_topics_file(p)

    def test_parse_rows_rejects_missing_field_with_line(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "t.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a.b","title":"no kind"}\n')
        with self.assertRaises(TxnError) as cm:
            topics.parse_topics_file(p)
        self.assertIn("line 1", str(cm.exception))


class TestMerge(unittest.TestCase):
    def test_same_kind_merges_aliases_first_title_wins(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3", "aliases": ["AWS S3"]},
            {"subject": "lake.s3", "kind": "entity", "title": "Amazon S3", "aliases": ["S3"]},
        ], {"source_id": "src_a", "version": 1}))
        e = topics.get(wiki, "lake.s3")
        self.assertEqual(e["title"], "S3")
        self.assertEqual(sorted(e["aliases"]), ["AWS S3", "S3"])

    def test_kind_conflict_keeps_first_kind(self):
        wiki = fresh()
        out = {}
        def go(t):
            out.update(topics.merge_rows(t, [
                {"subject": "lake.delta", "kind": "entity", "title": "Delta"},
                {"subject": "lake.delta", "kind": "concept", "title": "Delta"},
            ], {"source_id": "src_a", "version": 1}))
        _commit(wiki, go)
        e = topics.get(wiki, "lake.delta")
        self.assertEqual(e["kind"], "entity")
        self.assertEqual(e["status"], "conflict")
        self.assertEqual(e["alternate_kinds"], ["concept"])
        self.assertEqual(out["kind_conflicts"], ["lake.delta"])

    def test_auto_register_creates_concept(self):
        wiki = fresh()
        out = {}
        def go(t):
            out.update(topics.auto_register(t, ["misc.thing"],
                                            {"source_id": "src_a", "version": 1}))
        _commit(wiki, go)
        e = topics.get(wiki, "misc.thing")
        self.assertEqual(e["kind"], "concept")
        self.assertTrue(e["auto_registered"])
        self.assertEqual(out["auto_registered"], ["misc.thing"])

    def test_decision_kind_never_conflicts_with_merge(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "policy.pg", "kind": "decision", "title": "Use PG"},
        ], {"source_id": "src_a", "version": 1}))
        self.assertEqual(topics.get(wiki, "policy.pg")["kind"], "decision")

    def test_two_conflicts_in_one_txn_both_reach_review_queue(self):
        """Regression: review.add_item must compose over the txn's staged
        queue — a second conflict in the same merge must not drop the first."""
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.delta", "kind": "entity", "title": "Delta"},
            {"subject": "lake.delta", "kind": "concept", "title": "Delta"},
            {"subject": "lake.ice", "kind": "entity", "title": "Iceberg"},
            {"subject": "lake.ice", "kind": "procedure", "title": "Iceberg"},
        ], {"source_id": "src_a", "version": 1}))
        with open(wiki.p(".state", "review-queue.jsonl")) as f:
            items = [json.loads(l) for l in f if l.strip()]
        conflicts = [i for i in items if i["kind"] == "topic_kind_conflict"]
        self.assertEqual(len(conflicts), 2)
        self.assertEqual(sorted(i["affected"][0] for i in conflicts),
                         ["lake.delta", "lake.ice"])


class TestRegistryFile(unittest.TestCase):
    def test_registry_json_shape_and_version(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3"},
        ], {"source_id": "src_a", "version": 1}))
        reg = topics.load(wiki)
        self.assertEqual(reg["version"], 1)
        self.assertIn("lake.s3", reg["topics"])
        self.assertEqual(reg["topics"]["lake.s3"]["declared_by"],
                         [{"source_id": "src_a", "version": 1}])


class TestCompileTopics(unittest.TestCase):
    def _run_umbrella(self, wiki, topics_rows=None, cands=None):
        """Full umbrella: compile -> write files -> resume."""
        r = sources.ingest(wiki, "text", "doc-t", b"# Doc\nS3 is object storage.")
        plan = wc_compile.compile_umbrella(wiki)
        meta = plan["pending"][0]
        if topics_rows is not None:
            with open(meta["topics_path"], "w") as f:
                for row in topics_rows:
                    f.write(json.dumps(row) + "\n")
        with open(meta["candidates_path"], "w") as f:
            for c in (cands or []):
                f.write(json.dumps(c) + "\n")
        res = wc_compile.compile_umbrella(wiki, resume=True)
        while res["phase"] == "classify":
            cls = [{"index": row["index"], "relationship": "CONTRADICTION",
                    "target_claim_id": None, "authority": {"type": "authoritative_source"}}
                   for row in res["needs_review"]]
            with open(meta["classifications_path"], "w") as f:
                json.dump(cls, f)
            res = wc_compile.compile_umbrella(wiki, resume=True)
        return r, res

    def test_pending_entry_has_topics_path(self):
        wiki = fresh()
        sources.ingest(wiki, "text", "doc-t", b"hello")
        plan = wc_compile.compile_umbrella(wiki)
        self.assertIn("topics_path", plan["pending"][0])
        self.assertTrue(plan["pending"][0]["topics_path"].endswith("topics.jsonl"))

    def test_declared_topics_registered_and_in_receipt(self):
        wiki = fresh()
        cand = {"subject": "lake.s3", "predicate": "type", "value": "object store",
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "lake.s3", "kind": "entity", "title": "S3"}],
            cands=[cand])
        t = topics.get(wiki, "lake.s3")
        self.assertEqual(t["kind"], "entity")
        self.assertEqual(res["topics"]["registered"], 1)

    def test_source_topic_auto_registered(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki, topics_rows=[], cands=[])
        reg = topics.load(wiki)["topics"]
        self.assertTrue(any(e["kind"] == "source" for e in reg.values()))

    def test_undeclared_subject_auto_registered_with_warning(self):
        wiki = fresh()
        cand = {"subject": "misc.thing", "predicate": "p", "value": 1,
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki, topics_rows=[], cands=[cand])
        e = topics.get(wiki, "misc.thing")
        self.assertTrue(e["auto_registered"])
        self.assertIn("misc.thing", res["topics"]["auto_registered"])

    def test_missing_topics_file_warns_not_fails(self):
        wiki = fresh()
        cand = {"subject": "x.y", "predicate": "p", "value": 1,
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki, topics_rows=None, cands=[cand])
        self.assertEqual(res["phase"], "done")
        self.assertTrue(any("no_topics_declared" in w
                            for w in res["topics"]["warnings"]))

    def test_unclaimed_topic_listed(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "lake.presto", "kind": "entity", "title": "P"}],
            cands=[])
        self.assertIn("lake.presto", res["topics"]["unclaimed"])

    def test_decision_topic_in_receipt_not_page(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "policy.pg", "kind": "decision", "title": "PG"}],
            cands=[])
        self.assertIn("policy.pg", res["topics"]["decision_topics"])
        self.assertEqual(topics.get(wiki, "policy.pg")["kind"], "decision")


if __name__ == "__main__":
    unittest.main()
