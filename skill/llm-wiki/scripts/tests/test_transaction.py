import errno, json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore.store import Wiki, init_wiki
from wikicore.transaction import (
    ConflictError,
    LockedError,
    Transaction,
    TxnError,
)


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "txn-test")
    return wiki


class TestTransaction(unittest.TestCase):
    def test_happy_path_commit(self):
        wiki = fresh()
        txn = Transaction(wiki, "wiki-test")
        txn.stage_write("notes/hello.md", "hi")
        txn.stage_write("claims/x.json", {"a": 1})
        receipt = txn.commit(base_revision=0)
        self.assertEqual(receipt["revision"], {"before": 0, "after": 1})
        self.assertEqual(receipt["command"], "wiki-test")
        self.assertTrue(os.path.isfile(wiki.p("notes/hello.md")))
        with open(wiki.p("notes/hello.md")) as f:
            self.assertEqual(f.read(), "hi")
        with open(wiki.p("claims/x.json")) as f:
            self.assertEqual(json.load(f), {"a": 1})
        self.assertEqual(wiki.revision(), 1)
        ops = wiki.read_jsonl(".state/operations.jsonl")
        self.assertEqual(len(ops), 1)
        self.assertTrue(os.path.isfile(wiki.p(".state/operations/%s.receipt.json" % receipt["run_id"])))

    def test_stale_revision_conflict(self):
        wiki = fresh()
        txn = Transaction(wiki, "wiki-test")
        txn.stage_write("notes/a.md", "a")
        txn.commit(0)
        txn2 = Transaction(wiki, "wiki-test")
        txn2.stage_write("notes/b.md", "b")
        with self.assertRaises(ConflictError):
            txn2.commit(0)  # current is 1 now
        self.assertFalse(os.path.exists(wiki.p("notes/b.md")))

    def test_validator_blocks_commit(self):
        wiki = fresh()

        def bad_validator(txn):
            raise TxnError("intentional")

        Transaction.VALIDATORS.append(bad_validator)
        try:
            txn = Transaction(wiki, "wiki-test")
            txn.stage_write("notes/a.md", "a")
            with self.assertRaises(TxnError):
                txn.validate()
            with self.assertRaises(TxnError):
                txn.commit(0)
            self.assertFalse(os.path.exists(wiki.p("notes/a.md")))
        finally:
            Transaction.VALIDATORS.remove(bad_validator)

    def test_lock_rejects_concurrent_commit(self):
        import fcntl

        wiki = fresh()
        os.makedirs(wiki.p(".state/locks"), exist_ok=True)
        lock_fd = open(wiki.p(".state/locks/commit.lock"), "w")
        fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            txn = Transaction(wiki, "wiki-test")
            txn.stage_write("notes/a.md", "a")
            with self.assertRaises(LockedError):
                txn.commit(0)
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
            lock_fd.close()

    def test_failed_commit_leaves_staging(self):
        wiki = fresh()
        txn = Transaction(wiki, "wiki-test")
        txn.stage_write("notes/a.md", "a")
        # simulate failure after staging: wrong base revision then validator failure
        try:
            txn.commit(99)
        except ConflictError:
            pass
        staging = wiki.p(".state/staging")
        leftovers = [d for d in os.listdir(staging) if d.startswith("run_")]
        self.assertEqual(len(leftovers), 1)
        self.assertFalse(os.path.exists(wiki.p("notes/a.md")))

    def test_stage_delete(self):
        wiki = fresh()
        with open(wiki.p("notes/doomed.md"), "w") as f:
            f.write("x")
        txn = Transaction(wiki, "wiki-test")
        txn.stage_delete("notes/doomed.md")
        txn.commit(0)
        self.assertFalse(os.path.exists(wiki.p("notes/doomed.md")))


if __name__ == "__main__":
    unittest.main()
