"""SP2b Tasks 1-2: graph-export + viz-serve — deterministic gates.

graph-export: self-contained static bundle, byte-stable modulo generated_at,
no absolute network refs (offline contract).
viz-serve: loopback-only bind, live graph-data.json, SSE hello + reload on
store change. In-process server on an ephemeral port; no browser needed.
"""
import json
import os
import re
import shutil
import socket
import sys
import tempfile
import threading
import time
import unittest
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import claims, ids, vizexport, vizserve  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import Transaction  # noqa: E402


def _seed(n=2):
    w = Wiki(tempfile.mkdtemp(prefix="viz-"))
    init_wiki(w, "viz-test")
    txn = Transaction(w, "seed")
    for i in range(n):
        claims.save_claim(txn, {"id": ids.new("claim"), "status": "accepted",
                                "evidence": [], "subject": "svc.%d" % i,
                                "predicate": "uses database",
                                "value": "PostgreSQL",
                                "authority": {"type": "authoritative_source"}})
    txn.commit(w.revision())
    return w


def _tree(d):
    out = {}
    for root, _, files in os.walk(d):
        for fn in sorted(files):
            p = os.path.join(root, fn)
            out[os.path.relpath(p, d)] = open(p, "rb").read()
    return out


class TestGraphExport(unittest.TestCase):
    def test_bundle_shape_and_determinism(self):
        w = _seed()
        self.addCleanup(shutil.rmtree, w.root, ignore_errors=True)
        out1 = w.root + "-export1"
        out2 = w.root + "-export2"
        self.addCleanup(shutil.rmtree, out1, ignore_errors=True)
        self.addCleanup(shutil.rmtree, out2, ignore_errors=True)
        r = vizexport.export(w, out1)
        self.assertGreater(r["files"], 2)
        vizexport.export(w, out2)
        t1, t2 = _tree(out1), _tree(out2)
        self.assertEqual(set(t1), set(t2))
        for name in t1:
            if name == "graph-data.json":
                strip = lambda b: re.sub(
                    rb'"generated_at": "[^"]*"', b"", b)
                self.assertEqual(strip(t1[name]), strip(t2[name]))
            else:
                self.assertEqual(t1[name], t2[name],
                                 "non-payload file not byte-stable: %s" % name)
        payload = json.loads(t1["graph-data.json"])
        self.assertEqual(len(payload["nodes"]), r["nodes"])

    def test_offline_no_network_refs(self):
        w = _seed()
        self.addCleanup(shutil.rmtree, w.root, ignore_errors=True)
        out = w.root + "-export"
        self.addCleanup(shutil.rmtree, out, ignore_errors=True)
        vizexport.export(w, out)
        html = open(os.path.join(out, "index.html")).read()
        self.assertFalse(
            re.search(r'(?:src|href)\s*=\s*["\']https?://', html),
            "index.html must not reference network URLs (offline contract)")

    def test_empty_wiki_errors_no_partial_dir(self):
        w = _seed(0)
        self.addCleanup(shutil.rmtree, w.root, ignore_errors=True)
        out = w.root + "-export"
        from wikicore.transaction import TxnError
        with self.assertRaises(TxnError):
            vizexport.export(w, out)
        self.assertFalse(os.path.exists(out))


class TestVizServe(unittest.TestCase):
    def setUp(self):
        self.wiki = _seed()
        self.addCleanup(shutil.rmtree, self.wiki.root, ignore_errors=True)
        # ephemeral port: bind 0 via a scratch server, then reuse the number
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        self.port = s.getsockname()[1]
        s.close()
        self.srv, self.url = vizserve.serve(
            self.wiki, port=self.port, no_open=True, run_forever=False)
        self.addCleanup(self.srv.server_close)
        t = threading.Thread(target=self.srv.serve_forever, daemon=True)
        t.start()
        self.addCleanup(self.srv.shutdown)

    def _get(self, path):
        with urllib.request.urlopen(self.url.rstrip("/") + path, timeout=5) as r:
            return r.status, r.read()

    def test_loopback_bind_and_index(self):
        self.assertEqual(self.srv.server_address[0], "127.0.0.1")
        code, body = self._get("/")
        self.assertEqual(code, 200)
        self.assertIn(b"root", body)  # index.html served

    def test_graph_data_live_and_fresh_after_mutation(self):
        code, body = self._get("/graph-data.json")
        self.assertEqual(code, 200)
        n0 = len(json.loads(body)["nodes"])
        txn = Transaction(self.wiki, "grow")
        claims.save_claim(txn, {"id": ids.new("claim"), "status": "accepted",
                                "evidence": [], "subject": "svc.extra",
                                "predicate": "uses database",
                                "value": "MySQL",
                                "authority": {"type": "authoritative_source"}})
        txn.commit(self.wiki.revision())
        deadline = time.time() + 8
        n1 = n0
        while time.time() < deadline:
            _, body = self._get("/graph-data.json")
            n1 = len(json.loads(body)["nodes"])
            if n1 > n0:
                break
            time.sleep(0.4)
        self.assertGreater(n1, n0, "graph-data.json never reflected the write")

    def test_sse_hello_then_reload(self):
        sock = socket.create_connection(("127.0.0.1", self.port), timeout=5)
        self.addCleanup(sock.close)
        sock.sendall(b"GET /events HTTP/1.1\r\nHost: 127.0.0.1\r\n"
                     b"Accept: text/event-stream\r\n\r\n")
        buf = b""

        def read_until(token, timeout_s):
            nonlocal buf
            deadline = time.time() + timeout_s
            while time.time() < deadline:
                if token in buf:
                    return True
                sock.settimeout(max(0.1, deadline - time.time()))
                try:
                    buf += sock.recv(65536)
                except socket.timeout:
                    break
            return token in buf

        self.assertTrue(read_until(b"event: hello", 5), "no SSE hello")
        txn = Transaction(self.wiki, "push")
        claims.save_claim(txn, {"id": ids.new("claim"), "status": "accepted",
                                "evidence": [], "subject": "svc.pushed",
                                "predicate": "uses database",
                                "value": "SQLite",
                                "authority": {"type": "authoritative_source"}})
        txn.commit(self.wiki.revision())
        self.assertTrue(read_until(b"event: reload", 10),
                        "no SSE reload after store change: %r" % buf[:300])
        self.assertIn(b'"subject"', buf)  # reload carries the fresh payload


if __name__ == "__main__":
    unittest.main()
