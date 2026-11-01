"""SP2a Task 8: deterministic OKF v0.2 export — TDD."""
import hashlib
import json
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import claims, ids, okf  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import Transaction, TxnError  # noqa: E402
from wikicore.yamlite import loads as yamlload  # noqa: E402


def _seed(n=3):
    w = Wiki(tempfile.mkdtemp(prefix="okf-"))
    init_wiki(w, "okf-test")
    txn = Transaction(w, "seed")
    for i in range(n):
        claims.save_claim(txn, {"id": ids.new("claim"), "status": "accepted",
                                "evidence": [], "subject": "svc.%d" % i,
                                "predicate": "uses database",
                                "value": "PostgreSQL",
                                "authority": {"type": "authoritative_source"}})
    txn.commit(w.revision())
    return w


def _front(path):
    text = open(path).read()
    return yamlload("---\n%s---\n" % text.split("---")[1])


def _hash(d):
    h = hashlib.sha256()
    for root, _, files in sorted(os.walk(d)):
        for fn in sorted(files):
            p = os.path.join(root, fn)
            h.update(os.path.relpath(p, d).encode())
            h.update(open(p, "rb").read())
    return h.hexdigest()


class TestOkf(unittest.TestCase):
    def test_export_shape_and_determinism(self):
        w = _seed()
        self.addCleanup(shutil.rmtree, w.root, ignore_errors=True)
        out1, out2 = w.root + "-okf1", w.root + "-okf2"
        self.addCleanup(shutil.rmtree, out1, ignore_errors=True)
        r = okf.export(w, out1)
        self.assertEqual(r["pages"], 3)
        idx = open(os.path.join(out1, "index.md")).read()
        self.assertIn('okf_version: "0.2"', idx)
        self.assertIn("(concepts/svc-0.md)", idx)
        page = os.path.join(out1, "concepts", "svc-0.md")
        front = _front(page)
        self.assertEqual(front["type"], "concept")
        self.assertEqual(front["title"], "svc.0")
        self.assertEqual(front["generated"]["by"], "llm-wiki")
        self.assertTrue(front["generated"]["at"])
        body = open(page).read()
        self.assertIn("`uses database` = **PostgreSQL**", body)
        side = json.load(open(os.path.join(out1, ".claims", "svc-0.json")))
        self.assertEqual(side[0]["predicate"], "uses database")
        self.assertEqual(side[0]["value"], "PostgreSQL")
        okf.export(w, out2)
        self.assertEqual(_hash(out1), _hash(out2))  # byte-stable re-export
        shutil.rmtree(out2, ignore_errors=True)

    def test_reexport_replaces_existing_dir(self):
        w = _seed()
        self.addCleanup(shutil.rmtree, w.root, ignore_errors=True)
        out = w.root + "-okf"
        self.addCleanup(shutil.rmtree, out, ignore_errors=True)
        okf.export(w, out)
        okf.export(w, out)  # no nesting, no partial dir
        self.assertTrue(os.path.isfile(os.path.join(out, "index.md")))
        self.assertFalse(os.path.exists(os.path.join(out, "okf")))

    def test_empty_wiki_errors_no_partial_dir(self):
        w = _seed(0)
        self.addCleanup(shutil.rmtree, w.root, ignore_errors=True)
        out = w.root + "-okf"
        with self.assertRaises(TxnError):
            okf.export(w, out)
        self.assertFalse(os.path.exists(out))

    def test_supersession_link_between_concepts(self):
        w = _seed(2)
        self.addCleanup(shutil.rmtree, w.root, ignore_errors=True)
        out = w.root + "-okf"
        self.addCleanup(shutil.rmtree, out, ignore_errors=True)
        by_subj = {c["subject"]: c for c in claims.list_claims(w)}
        c0, c1 = by_subj["svc.0"], by_subj["svc.1"]  # list order is not seed order
        txn = Transaction(w, "sup")
        claims.save_claim(txn, {"id": ids.new("claim"), "status": "superseded",
                                "superseded_by": c1["id"], "evidence": [],
                                "subject": c0["subject"],
                                "predicate": c0["predicate"],
                                "value": "MySQL",
                                "authority": {"type": "authoritative_source"}})
        txn.commit(w.revision())
        r = okf.export(w, out)
        self.assertGreaterEqual(r["links"], 1)
        body = open(os.path.join(out, "concepts", "svc-0.md")).read()
        self.assertIn("(svc-1.md)", body)


if __name__ == "__main__":
    unittest.main()
