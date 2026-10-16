import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, sources, topics
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


if __name__ == "__main__":
    unittest.main()
