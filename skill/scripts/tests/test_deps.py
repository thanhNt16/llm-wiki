import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import deps
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "deps-test")
    return wiki


class TestDeps(unittest.TestCase):
    def test_register_and_invalidate(self):
        wiki = fresh()
        txn = Transaction(wiki, "t")
        deps.register(txn, "wiki/concepts/attribution.md",
                      ["claim_01JABC@3", "decision_01JDEF@2"])
        txn.commit(0)
        stale = deps.invalidate(wiki, ["claim_01JABC"])
        self.assertEqual(stale, ["wiki/concepts/attribution.md"])
        self.assertTrue(deps.is_stale(wiki, "wiki/concepts/attribution.md"))
        self.assertFalse(deps.is_stale(wiki, "wiki/index.md"))

    def test_transitive_invalidation(self):
        wiki = fresh()
        txn = Transaction(wiki, "t1")
        deps.register(txn, "wiki/concepts/attr.md", ["claim_01JABC@3"])
        txn.commit(0)
        txn = Transaction(wiki, "t2")
        deps.register(txn, "context/ctx1.md", ["wiki/concepts/attr.md"])
        txn.commit(1)
        stale = deps.invalidate(wiki, ["claim_01JABC"])
        self.assertIn("wiki/concepts/attr.md", stale)
        self.assertIn("context/ctx1.md", stale)

    def test_invalidate_any_version(self):
        wiki = fresh()
        txn = Transaction(wiki, "t")
        deps.register(txn, "wiki/decisions/d1.md", ["decision_01JDEF@1"])
        txn.commit(0)
        # dependency was registered at version 1; bump to 5 still invalidates
        stale = deps.invalidate(wiki, ["decision_01JDEF"])
        self.assertEqual(stale, ["wiki/decisions/d1.md"])

    def test_invalidate_unknown_id_noop(self):
        wiki = fresh()
        self.assertEqual(deps.invalidate(wiki, ["claim_MISSING"]), [])

    def test_clear_stale(self):
        wiki = fresh()
        txn = Transaction(wiki, "t")
        deps.register(txn, "wiki/a.md", ["claim_01JABC@1"])
        deps.register(txn, "wiki/b.md", ["claim_01JABC@1"])
        txn.commit(0)
        deps.invalidate(wiki, ["claim_01JABC"])
        txn2 = Transaction(wiki, "t2")
        deps.clear_stale(txn2, ["wiki/a.md"])
        txn2.commit(1)
        self.assertFalse(deps.is_stale(wiki, "wiki/a.md"))
        self.assertTrue(deps.is_stale(wiki, "wiki/b.md"))


if __name__ == "__main__":
    unittest.main()
