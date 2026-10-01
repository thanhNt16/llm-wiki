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
        self.assertEqual(by["node_modules/x.js"]["reason"], "vcs/vendor dir")
        self.assertEqual(by["big.bin"]["reason"], "oversize")
        self.assertEqual(by["a.zip"]["reason"], "non-evidence binary")
        self.assertEqual(by[".DS_Store"]["reason"], "non-evidence binary")

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
        p = wiki.p("state", "ingest-batches", "%s.json" % r["batch_id"])
        self.assertTrue(os.path.isfile(p))
        with open(p) as f:
            saved = json.load(f)
        self.assertEqual(saved["batch_id"], r["batch_id"])

    def test_dir_must_be_directory(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n"})
        with self.assertRaises(TxnError):
            dirs.ingest_dir(wiki, os.path.join(d, "a.md"))


if __name__ == "__main__":
    unittest.main()
