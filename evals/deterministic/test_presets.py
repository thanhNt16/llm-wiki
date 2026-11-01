"""SP3-T4: declarative connector presets (github-issues, rest) — gates.

Network-free: the only HTTP is a ThreadingHTTPServer bound to 127.0.0.1
port 0, and the github-issues auth test injects a recording fetch stub.
"""
import json
import os
import shutil
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import connectors  # standalone import pins the cycle guard
from wikicore import sources  # noqa: E402
from wikicore.shorthand import normalize_candidate  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import TxnError  # noqa: E402


def _wiki():
    w = Wiki(tempfile.mkdtemp(prefix="preset-"))
    init_wiki(w, "preset-test")
    return w


def _clean(w):
    return lambda: shutil.rmtree(w.root, True)


class _JSONHandler(BaseHTTPRequestHandler):
    payload = b"{}"

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(self.payload)))
        self.end_headers()
        self.wfile.write(self.payload)

    def log_message(self, *a):
        pass

class _RedirectThenRecord(BaseHTTPRequestHandler):
    """Any path except /final 302s to `target`; /final records headers."""
    target = "/final"
    body = b"{}"
    seen = []

    def do_GET(self):
        if self.path == "/final":
            type(self).seen.append(dict(self.headers))
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(self.body)))
            self.end_headers()
            self.wfile.write(self.body)
        else:
            self.send_response(302)
            self.send_header("Location", type(self).target)
            self.send_header("Content-Length", "0")
            self.end_headers()

    def log_message(self, *a):
        pass



def _serve(payload: bytes):
    handler = type("H", (_JSONHandler,), {"payload": payload})
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    return httpd, "http://127.0.0.1:%d/items" % httpd.server_address[1]


class TestPresetRegistry(unittest.TestCase):
    def test_preset_registry_lists(self):
        names = set(connectors.list_presets())
        self.assertTrue({"github-issues", "rest"} <= names)

    def test_unknown_preset_raises(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        with self.assertRaises(TxnError):
            connectors.ingest_preset(w, "nope", "http://127.0.0.1/x",
                                     fetch=lambda u, h: b"{}")


class TestRestPreset(unittest.TestCase):
    FIXTURE = json.dumps(
        {"items": [{"title": "Deploy failed", "body": "api 500", "id": 42}]}
    ).encode()

    def test_rest_preset_ingests_fixture(self):
        httpd, url = _serve(self.FIXTURE)
        self.addCleanup(lambda: (httpd.shutdown(), httpd.server_close()))
        w = _wiki()
        self.addCleanup(_clean(w))

        r = connectors.ingest_preset(w, "rest", url)  # default fetch, loopback
        self.assertEqual(r["new_in_store"], 1)
        self.assertIsNotNone(r["source_id"])
        text = sources.load_content(w, r["source_id"], r["version"])
        self.assertIn("Deploy failed", text)   # candidate-shaped subject
        self.assertIn("api 500", text)         # captured body content

        r2 = connectors.ingest_preset(w, "rest", url)  # dedup on re-run
        self.assertEqual(r2["new_in_store"], 0)
        self.assertEqual(r2["source_id"], r["source_id"])


class TestGithubIssuesPreset(unittest.TestCase):
    def test_github_issues_mapping(self):
        item = {"number": 7, "title": "Crash on import", "state": "open",
                "body": "stack trace..."}
        c = connectors._map_github_issue(item)
        self.assertEqual(c["subject"], "issue #7: Crash on import")
        self.assertEqual(c["predicate"], "state")
        self.assertEqual(c["value"], "open")
        self.assertIn("stack trace...", json.dumps(c))  # body kept as content
        # every key must survive the T2 allowlist (no 'quote' or other
        # dead fields) — normalize_candidate raises on unknown keys
        n = normalize_candidate(dict(c), "source_x", 1, "preset:test")
        self.assertIn("stack trace...",
                      n["evidence"][0]["locator"]["value"])

    def test_env_auth_header_never_leaks(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        token = "ghp_unittest_deadbeef123"
        seen = {}

        def stub(url, headers):
            seen["url"] = url
            seen["headers"] = dict(headers)
            return json.dumps([{"number": 7, "title": "Crash on import",
                                "state": "open", "body": "boom"}]).encode()

        with patch.dict(os.environ, {"GITHUB_TOKEN": token}):
            r = connectors.ingest_preset(
                w, "github-issues",
                "https://api.github.com/repos/x/y/issues", fetch=stub)
        self.assertEqual(seen["headers"].get("Authorization"),
                         "Bearer " + token)
        self.assertNotIn(token, json.dumps(r))
        self.assertNotIn(token, sources.load_content(w, r["source_id"],
                                                     r["version"]))

    def test_no_token_no_auth_header(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        seen = {}

        def stub(url, headers):
            seen["headers"] = dict(headers)
            return json.dumps([{"number": 1, "title": "t", "state": "open",
                                "body": "b"}]).encode()

        env = {k: v for k, v in os.environ.items() if k != "GITHUB_TOKEN"}
        with patch.dict(os.environ, env, clear=True):
            connectors.ingest_preset(w, "github-issues",
                                     "https://api.github.com/x", fetch=stub)
        self.assertNotIn("Authorization", seen["headers"])

    def test_pr_items_skipped(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        payload = json.dumps([
            {"number": 3, "title": "Fix it", "state": "open",
             "body": "real issue"},
            {"number": 4, "title": "Add feature", "state": "open",
             "body": "this is a PR", "pull_request": {"url": "..."}},
        ]).encode()
        r = connectors.ingest_preset(
            w, "github-issues", "https://api.github.com/x",
            fetch=lambda u, h: payload)
        self.assertEqual(r["items"], 2)
        self.assertEqual(r["new_in_store"], 1)
        self.assertEqual(r["skipped"], 1)
        text = sources.load_content(w, r["source_id"], r["version"])
        self.assertIn("Fix it", text)
        self.assertNotIn("Add feature", text)

    def test_auth_scoped_to_github_hosts(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        seen = []

        def stub(url, headers):
            seen.append((url, dict(headers)))
            return json.dumps([{"number": 1, "title": "t", "state": "open",
                                "body": "b"}]).encode()

        env = dict(os.environ, GITHUB_TOKEN="ghp_unittest_deadbeef123")
        with patch.dict(os.environ, env, clear=True):
            for url in ("https://api.github.com/repos/x/y/issues",
                        "https://github.example.com/x",
                        "https://evilgithub.com/x",
                        "https://not-ghe.com/x"):
                connectors.ingest_preset(w, "github-issues", url, fetch=stub)
        by_url = dict(seen)
        self.assertEqual(by_url["https://api.github.com/repos/x/y/issues"]
                         .get("Authorization"), "Bearer " + env["GITHUB_TOKEN"])
        for url in ("https://github.example.com/x",
                    "https://evilgithub.com/x", "https://not-ghe.com/x"):
            self.assertNotIn("Authorization", by_url[url])


class TestPresetFetchSecurity(unittest.TestCase):
    def _redirect_pair(self, cross_origin: bool):
        """Server A redirects to server B (cross) or to itself (same)."""
        rec = type("Rec", (_RedirectThenRecord,), {"seen": []})
        httpd_b = ThreadingHTTPServer(("127.0.0.1", 0), rec)
        threading.Thread(target=httpd_b.serve_forever, daemon=True).start()
        self.addCleanup(lambda: (httpd_b.shutdown(), httpd_b.server_close()))
        if cross_origin:
            target = "http://127.0.0.1:%d/final" % httpd_b.server_address[1]
            redir = type("Redir", (_RedirectThenRecord,),
                         {"target": target, "seen": []})
            httpd_a = ThreadingHTTPServer(("127.0.0.1", 0), redir)
            threading.Thread(target=httpd_a.serve_forever, daemon=True).start()
            self.addCleanup(lambda: (httpd_a.shutdown(),
                                     httpd_a.server_close()))
        else:
            httpd_a = httpd_b          # same server, same origin
        return rec, "http://127.0.0.1:%d/start" % httpd_a.server_address[1]

    def test_cross_origin_redirect_strips_authorization(self):
        rec, url = self._redirect_pair(cross_origin=True)
        connectors._default_fetch(url, {"Authorization": "Bearer leakme"})
        self.assertEqual(len(rec.seen), 1)
        self.assertNotIn("Authorization", rec.seen[0])

    def test_same_origin_redirect_keeps_authorization(self):
        rec, url = self._redirect_pair(cross_origin=False)
        connectors._default_fetch(url, {"Authorization": "Bearer keepme"})
        self.assertEqual(len(rec.seen), 1)
        self.assertEqual(rec.seen[0].get("Authorization"), "Bearer keepme")


class TestRestShapeGuards(unittest.TestCase):
    def test_non_dict_body_raises_txn_error(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        for body in (b"[]", b"[1,2]", b'"x"', b"7"):
            with self.assertRaises(TxnError, msg="body %r" % body):
                connectors.ingest_preset(w, "rest", "http://127.0.0.1/x",
                                         fetch=lambda u, h: body)

    def test_null_id_falls_back_to_number(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        # github-issues: `id` is not a required key, so id:null + number
        # items reach item_id — two of them must NOT collide on '#None'
        payload = json.dumps([
            {"id": None, "number": 7, "title": "a", "state": "open"},
            {"id": None, "number": 8, "title": "c", "state": "open"},
        ]).encode()
        r = connectors.ingest_preset(w, "github-issues",
                                     "https://api.github.com/x",
                                     fetch=lambda u, h: payload)
        self.assertEqual(r["new_in_store"], 2)  # distinct #7/#8 keys, not #None
        self.assertNotEqual(r["sources"][0], r["sources"][1])

    def test_no_id_or_number_raises(self):
        w = _wiki()
        self.addCleanup(_clean(w))
        payload = json.dumps({"items": [
            {"id": 0, "title": "a", "body": "b"},  # falsy id, no number
        ]}).encode()
        with self.assertRaises(TxnError):
            connectors.ingest_preset(w, "rest", "http://127.0.0.1/x",
                                     fetch=lambda u, h: payload)



if __name__ == "__main__":
    unittest.main()
