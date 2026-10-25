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

    def test_commit_clears_journal_and_backups(self):
        wiki = fresh()
        txn = Transaction(wiki, "t1")
        txn.stage_write("notes/a.md", "original-a")
        txn.commit(0)
        txn = Transaction(wiki, "t2")
        txn.stage_write("notes/a.md", "updated-a")
        txn.commit(wiki.revision())
        self.assertFalse(os.path.exists(wiki.p(".state/journal.json")))
        leftovers = [n for n in os.listdir(wiki.p(".state")) if n.startswith(".backup-")]
        self.assertEqual(leftovers, [])

    def test_mid_commit_failure_restores_backups(self):
        wiki = fresh()
        txn = Transaction(wiki, "t1")
        txn.stage_write("notes/a.md", "original-a")
        txn.commit(0)
        txn = Transaction(wiki, "t2")
        txn.stage_write("notes/a.md", "updated-a")
        txn.stage_write("notes/b.md", "new-b")
        real_replace = os.replace
        calls = []

        def flaky_replace(src, dst):
            calls.append(dst)
            if len(calls) == 2:  # fail on the second staged write
                raise OSError("boom mid-commit")
            return real_replace(src, dst)

        from wikicore import transaction as tmod
        tmod.os.replace = flaky_replace
        try:
            with self.assertRaises(OSError):
                txn.commit(wiki.revision())
        finally:
            tmod.os.replace = real_replace
        with open(wiki.p("notes/a.md")) as f:
            self.assertEqual(f.read(), "original-a")
        self.assertFalse(os.path.exists(wiki.p("notes/b.md")))
        self.assertEqual(wiki.revision(), 1)  # failed commit must not bump
        # journal left in place for doctor forensics
        self.assertTrue(os.path.isfile(wiki.p(".state/journal.json")))
        journal = json.load(open(wiki.p(".state/journal.json")))
        self.assertEqual(journal["receipt"]["command"], "t2")

    def test_raw_json_evidence_not_validated(self):
        # raw/ evidence is arbitrary bytes: a non-JSON *.json file there
        # must not trip the canonical-JSON validator
        wiki = fresh()
        txn = Transaction(wiki, "wiki-test")
        txn.stage_write("raw/file/abc123/report.json", "{ not json at all")
        receipt = txn.commit(0)
        self.assertEqual(receipt["revision"]["after"], 1)
        with open(wiki.p("raw/file/abc123/report.json")) as f:
            self.assertEqual(f.read(), "{ not json at all")

    def test_canonical_json_still_validated(self):
        wiki = fresh()
        txn = Transaction(wiki, "wiki-test")
        txn.stage_write("claims/claim_x.json", "{ broken")
        with self.assertRaises(TxnError):
            txn.commit(0)

    def test_failed_commit_writes_per_run_journal(self):
        # a crashed commit leaves BOTH the doctor-visible journal.json and a
        # run-scoped journal-<run>.json the next crash cannot clobber
        wiki = fresh()
        txn = Transaction(wiki, "t-crash")
        txn.stage_write("notes/a.md", "a")
        real_replace = os.replace

        def boom(src, dst):
            raise OSError("crash mid-commit")

        from wikicore import transaction as tmod
        tmod.os.replace = boom
        try:
            with self.assertRaises(OSError):
                txn.commit(wiki.revision())
        finally:
            tmod.os.replace = real_replace
        run_journal = wiki.p(".state/journal-%s.json" % txn.run_id)
        self.assertTrue(os.path.isfile(run_journal))
        self.assertTrue(os.path.isfile(wiki.p(".state/journal.json")))
        journal = json.load(open(run_journal))
        self.assertEqual(journal["receipt"]["command"], "t-crash")
        # a second successful commit clears its own journals without touching
        # the earlier crash's per-run record... (journal.json is the latest
        # leftover alias and is removed on success)
        txn2 = Transaction(wiki, "t-ok")
        txn2.stage_write("notes/b.md", "b")
        txn2.commit(wiki.revision())
        self.assertTrue(os.path.isfile(run_journal))  # crash record survives
        self.assertFalse(os.path.exists(wiki.p(".state/journal.json")))
        self.assertFalse(
            os.path.exists(wiki.p(".state/journal-%s.json" % txn2.run_id)))

    def test_lock_symlink_refused(self):
        # O_NOFOLLOW: a planted symlink at the lock path must not be followed
        wiki = fresh()
        lock = wiki.p(".state/locks/commit.lock")
        os.makedirs(os.path.dirname(lock), exist_ok=True)
        os.symlink("/tmp/commit.lock.target", lock)
        txn = Transaction(wiki, "wiki-test")
        txn.stage_write("notes/a.md", "a")
        try:
            with self.assertRaises(OSError):
                txn.commit(0)
        finally:
            os.unlink(lock)

    def test_operations_jsonl_torn_tail_repaired(self):
        # a mid-append crash leaves a corrupt final line; the next commit
        # amputates it so read_jsonl never sees it
        wiki = fresh()
        ops = wiki.p(".state/operations.jsonl")
        txn = Transaction(wiki, "t1")
        txn.stage_write("notes/a.md", "a")
        txn.commit(0)
        with open(ops, "a") as f:
            f.write('{"run_id": "run_TORN", "comman')  # torn write
        txn2 = Transaction(wiki, "t2")
        txn2.stage_write("notes/b.md", "b")
        txn2.commit(wiki.revision())
        records = wiki.read_jsonl(".state/operations.jsonl")
        self.assertEqual(len(records), 2)
        self.assertEqual([r["command"] for r in records], ["t1", "t2"])


if __name__ == "__main__":
    unittest.main()
