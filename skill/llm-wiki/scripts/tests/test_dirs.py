import json, os, sys, tempfile, unittest
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import dirs, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import TxnError


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "dir-test")
    return wiki


def tree(files):
    """files: {relpath: bytes}; returns dir path."""
    root = tempfile.mkdtemp()
    for rel, data in files.items():
        p = os.path.join(root, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            f.write(data)
    return root


class TestIngestDir(unittest.TestCase):
    def test_walks_nested_tree_one_source_per_file(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "sub/b.md": b"b\n", "sub/deep/c.md": b"c\n"})
        r = dirs.ingest_dir(wiki, d)
        self.assertEqual(r["counts"]["ingested"], 3)
        ids = {i["source_id"] for i in r["items"]}
        self.assertEqual(len(ids), 3)
        m = sources.get_manifest(wiki, r["items"][0]["source_id"])
        self.assertEqual(m["root_origin"], "dir:" + os.path.abspath(d))

    def test_skip_rules(self):
        wiki = fresh()
        d = tree({
            "keep.md": b"k\n",
            ".hidden.md": b"h\n",
            "node_modules/x.js": b"js\n",
            ".git/config": b"g\n",
            "big.bin": b"x" * (1024),
            "a.zip": b"PK\x03\x04zip",
            ".DS_Store": b"junk",
        })
        r = dirs.ingest_dir(wiki, d, max_bytes=512)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["keep.md"]["status"], "ingested")
        self.assertEqual(by[".hidden.md"]["reason"], "hidden")
        # skipped dirs are pruned: the dir is recorded once, never descended
        self.assertEqual(by["node_modules"]["reason"], "vcs/vendor dir")
        self.assertEqual(by[".git"]["reason"], "vcs/vendor dir")
        self.assertNotIn("node_modules/x.js", by)
        self.assertNotIn(".git/config", by)

    def test_include_hidden(self):
        wiki = fresh()
        d = tree({".env.example": b"A=1\n", "x.md": b"x\n"})
        r = dirs.ingest_dir(wiki, d, include_hidden=True)
        self.assertEqual(r["counts"]["ingested"], 2)

    def test_rerun_dedups_then_force(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "b.md": b"b\n"})
        dirs.ingest_dir(wiki, d)
        r2 = dirs.ingest_dir(wiki, d)
        self.assertEqual(r2["counts"]["deduplicated"], 2)
        r3 = dirs.ingest_dir(wiki, d, force=True)
        self.assertEqual(r3["counts"]["ingested"], 2)
        sid = next(i["source_id"] for i in r3["items"])
        self.assertEqual(len(sources.get_manifest(wiki, sid)["versions"]), 2)

    def test_changed_file_new_version_only(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "b.md": b"b\n"})
        dirs.ingest_dir(wiki, d)
        with open(os.path.join(d, "a.md"), "w") as f:
            f.write("a changed\n")
        r = dirs.ingest_dir(wiki, d)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["a.md"]["status"], "ingested")
        self.assertEqual(by["a.md"]["version"], 2)
        self.assertEqual(by["b.md"]["status"], "deduplicated")

    def test_skips_wiki_dir_inside_tree(self):
        wiki = fresh()
        # a directory that contains a .llm-wiki store must not ingest it
        wiki2_root = tempfile.mkdtemp()
        os.makedirs(os.path.join(wiki2_root, ".llm-wiki"), exist_ok=True)
        with open(os.path.join(wiki2_root, ".llm-wiki", "x.json"), "w") as f:
            f.write("{}")
        with open(os.path.join(wiki2_root, "a.md"), "w") as f:
            f.write("a\n")
        r = dirs.ingest_dir(wiki, wiki2_root)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["a.md"]["status"], "ingested")
        self.assertNotIn(".llm-wiki/x.json", by)

    def test_unreadable_file_is_error_not_abort(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "b.md": b"b\n"})
        os.chmod(os.path.join(d, "b.md"), 0)
        try:
            r = dirs.ingest_dir(wiki, d)
        finally:
            os.chmod(os.path.join(d, "b.md"), 0o644)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["a.md"]["status"], "ingested")
        self.assertEqual(by["b.md"]["status"], "error")

    def test_batch_receipt_written(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n"})
        r = dirs.ingest_dir(wiki, d)
        p = wiki.p(".state", "ingest-batches", "%s.json" % r["batch_id"])
        self.assertTrue(os.path.isfile(p))
        with open(p) as f:
            saved = json.load(f)
        self.assertEqual(saved["batch_id"], r["batch_id"])

    def test_dir_must_be_directory(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n"})
        with self.assertRaises(TxnError):
            dirs.ingest_dir(wiki, os.path.join(d, "a.md"))

    def test_pruned_skip_dir_is_not_traversed(self):
        """Regression: _walk must remove skipped dirs from dirnames so
        os.walk never descends (node_modules at scale must not be walked)."""
        wiki = fresh()
        d = tree({"node_modules/dep/index.js": b"x\n", "a.md": b"a\n"})
        seen = []
        real_walk = os.walk

        def spy(*a, **kw):
            for triple in real_walk(*a, **kw):
                seen.append(triple[0])
                yield triple

        with mock.patch("os.walk", spy):
            r = dirs.ingest_dir(wiki, d)
        self.assertEqual(r["counts"]["ingested"], 1)
        self.assertFalse(any("node_modules" in p for p in seen),
                         "walk descended into a pruned skip dir")
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["node_modules"]["reason"], "vcs/vendor dir")

    def test_symlink_swap_between_walk_and_open_is_skipped(self):
        """Regression (TOCTOU): a file re-checked as a symlink at open time
        is skipped, not ingested."""
        wiki = fresh()
        d = tree({"a.md": b"a\n", "target.md": b"t\n"})
        real_walk = dirs._walk
        swapped = {"done": False}

        def walk_then_swap(*a, **kw):
            items = real_walk(*a, **kw)
            if not swapped["done"]:
                swapped["done"] = True
                os.unlink(os.path.join(d, "a.md"))
                os.symlink(os.path.join(d, "target.md"),
                           os.path.join(d, "a.md"))
            return items

        with mock.patch.object(dirs, "_walk", walk_then_swap):
            r = dirs.ingest_dir(wiki, d)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["a.md"]["status"], "skipped")
        self.assertEqual(by["a.md"]["reason"], "symlink")
        self.assertEqual(by["target.md"]["status"], "ingested")
        # the symlinked bytes must NOT be registered as an a.md source
        a_item = by["a.md"]
        self.assertNotIn("source_id", a_item)


if __name__ == "__main__":
    unittest.main()
