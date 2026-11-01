"""SP3-T10: CLI wiring for wave-1 modules — subprocess-level contracts.

Covers: global --wait-lock/--no-wait (exit 5 LockedError convention),
ingest --connector/--ref/--list-connectors, review triage/apply,
stage-candidates --run. Subprocess conventions follow test_evidence.py.
"""
import fcntl
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
WIKI = os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts", "wiki.py")
SCRIPTS = os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts")
sys.path.insert(0, SCRIPTS)

from wikicore import review as review_mod  # noqa: E402
from wikicore.store import Wiki  # noqa: E402
from wikicore.transaction import Transaction  # noqa: E402


def _run(root, *args):
    return subprocess.run([sys.executable, WIKI, "--root", root, *args],
                          capture_output=True, text=True)


def _fresh():
    root = tempfile.mkdtemp(prefix="cliwire-")
    p = _run(root, "init", "--name", "wire-test")
    assert p.returncode == 0, p.stdout + p.stderr
    return root


class _RestHandler(BaseHTTPRequestHandler):
    """Generic REST-preset fixture: {"items": [...]} with required keys."""

    def do_GET(self):
        body = json.dumps({"items": [
            {"id": 7, "title": "Deploy failed",
             "body": "deploy broke on friday"},
            {"id": 8, "title": "Cache warmed",
             "body": "redis cache pre-warmed"},
        ]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


def _serve(handler=_RestHandler):
    srv = HTTPServer(("127.0.0.1", 0), handler)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv, "http://127.0.0.1:%d" % srv.server_port


class _LockHolder:
    """Hold the wiki's commit.lock via flock until released()."""

    def __init__(self, root):
        self.path = os.path.join(root, ".llm-wiki", ".state", "locks",
                                 "commit.lock")
        os.makedirs(os.path.dirname(self.path), exist_ok=True)
        self.fd = os.open(self.path, os.O_RDWR | os.O_CREAT)
        fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def release(self):
        fcntl.flock(self.fd, fcntl.LOCK_UN)
        os.close(self.fd)


class TestIngestConnector(unittest.TestCase):
    def setUp(self):
        self.root = _fresh()
        self.addCleanup(lambda: shutil.rmtree(self.root, True))

    def test_list_connectors(self):
        p = _run(self.root, "ingest", "--list-connectors")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        out = json.loads(p.stdout)
        self.assertEqual(out["connectors"],
                         ["github-issues", "rest"])

    def test_rest_preset_happy_path(self):
        srv, base = _serve()
        self.addCleanup(lambda: (srv.shutdown(), srv.server_close()))
        p = _run(self.root, "ingest", "--connector", "rest",
                 "--ref", base + "/items", "--limit", "10")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        out = json.loads(p.stdout)
        self.assertEqual(out["preset"], "rest")
        self.assertEqual(out["items"], 2)
        self.assertEqual(out["new_in_store"], 2)
        self.assertTrue(out["source_id"].startswith("source_"))
        # dedup on re-run: same items are skipped, not re-ingested
        p2 = _run(self.root, "ingest", "--connector", "rest",
                  "--ref", base + "/items")
        self.assertEqual(p2.returncode, 0, p2.stdout + p2.stderr)
        self.assertEqual(json.loads(p2.stdout)["new_in_store"], 0)

    def test_unknown_preset_exit_3(self):
        p = _run(self.root, "ingest", "--connector", "nope",
                 "--ref", "http://127.0.0.1:1/x")
        self.assertEqual(p.returncode, 3, p.stdout)
        self.assertIn("unknown connector preset", p.stdout)

    def test_connector_requires_ref(self):
        p = _run(self.root, "ingest", "--connector", "rest")
        self.assertEqual(p.returncode, 3, p.stdout)


class TestReviewTriageApply(unittest.TestCase):
    def setUp(self):
        self.root = _fresh()
        self.addCleanup(lambda: shutil.rmtree(self.root, True))
        # seed one open item with no resolvable claim (suggests defer)
        wiki = Wiki(self.root)
        txn = Transaction(wiki, "seed")
        self.item_id = review_mod.add_item(
            txn, "needs_review", "orders_service ~ order.service",
            [], ["claim_deadbeef"], "medium", "agent adjudication required")
        txn.commit(wiki.revision())

    def test_triage_groups_and_suggests(self):
        p = _run(self.root, "review", "triage")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        out = json.loads(p.stdout)
        keys = [g["key"] for g in out["groups"]]
        self.assertEqual(keys, sorted(keys))
        rows = [r for g in out["groups"] for r in g["items"]]
        self.assertEqual([r["id"] for r in rows], [self.item_id])
        self.assertEqual(rows[0]["suggested_action"], "defer")
        # group-by override: bucket on a real item field
        p2 = _run(self.root, "review", "triage", "--group-by", "kind")
        self.assertEqual(p2.returncode, 0, p2.stdout + p2.stderr)
        self.assertEqual([g["key"] for g in json.loads(p2.stdout)["groups"]],
                         ["needs_review"])

    def test_apply_defers_item(self):
        actions = [{"id": self.item_id, "action": "defer", "params": {}}]
        af = os.path.join(self.root, "actions.json")
        with open(af, "w") as f:
            json.dump(actions, f)
        p = _run(self.root, "review", "apply", "--file", af)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        out = json.loads(p.stdout)
        self.assertEqual(out["applied"], 1)
        self.assertEqual(out["results"][0]["status"], "deferred")
        self.assertIsInstance(out["revision"], int)
        q = _run(self.root, "review", "list", "--status", "deferred")
        self.assertEqual(len(json.loads(q.stdout)["items"]), 1)

    def test_apply_stale_base_conflict_exit_4(self):
        actions = [{"id": self.item_id, "action": "defer", "params": {}}]
        af = os.path.join(self.root, "actions.json")
        with open(af, "w") as f:
            json.dump(actions, f)
        p = _run(self.root, "review", "apply", "--file", af,
                 "--base-revision", "0")
        self.assertEqual(p.returncode, 4, p.stdout)


class TestStageCandidatesRun(unittest.TestCase):
    def setUp(self):
        self.root = _fresh()
        self.addCleanup(lambda: shutil.rmtree(self.root, True))
        p = _run(self.root, "ingest", "--text", "postgres holds orders",
                 "--text-ref", "n1")
        self.assertEqual(p.returncode, 0, p.stdout)
        self.sid = json.loads(p.stdout)["source_id"]
        p = _run(self.root, "build")
        self.assertEqual(p.returncode, 3, p.stdout)
        pending = json.loads(p.stdout)["pending"][0]
        self.candidates_path = pending["candidates_path"]
        self.run_id = os.path.basename(os.path.dirname(self.candidates_path))

    def test_stage_into_named_run(self):
        cf = os.path.join(self.root, "cands.jsonl")
        with open(cf, "w") as f:
            f.write('{"subject": "postgres", "predicate": "role", '
                    '"value": "orders store", '
                    '"locator": "s:postgres holds orders"}\n')
        p = _run(self.root, "stage-candidates", "--file", cf,
                 "--source-id", self.sid, "--source-version", "1",
                 "--run", self.run_id)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(json.loads(p.stdout)["run_id"], self.run_id)
        # staged into the SAME run dir (not a fresh orphan)
        self.assertTrue(os.path.isfile(
            os.path.join(os.path.dirname(self.candidates_path),
                         "compile.json")))
        # build can now resume to completion
        p = _run(self.root, "build")
        self.assertEqual(p.returncode, 0, p.stdout)

    def test_unknown_run_exit_3(self):
        cf = os.path.join(self.root, "cands.jsonl")
        with open(cf, "w") as f:
            f.write('{"subject": "x", "predicate": "y", "value": "z"}\n')
        p = _run(self.root, "stage-candidates", "--file", cf,
                 "--source-id", self.sid, "--source-version", "1",
                 "--run", "run_deadbeefdeadbeefdeadbeef00")
        self.assertEqual(p.returncode, 3, p.stdout)


class TestWaitLock(unittest.TestCase):
    def setUp(self):
        self.root = _fresh()
        self.addCleanup(lambda: shutil.rmtree(self.root, True))

    def test_mutating_wait_times_out_exit_5(self):
        holder = _LockHolder(self.root)
        try:
            t0 = time.monotonic()
            p = _run(self.root, "--wait-lock", "0.4",
                     "ingest", "--text", "x", "--text-ref", "t")
            dt = time.monotonic() - t0
            self.assertEqual(p.returncode, 5, p.stdout + p.stderr)
            out = json.loads(p.stdout)
            self.assertIn("locks/commit.lock", out["error"])
            self.assertGreaterEqual(dt, 0.35)
        finally:
            holder.release()
        # released -> same command proceeds
        p = _run(self.root, "--wait-lock", "0.4",
                 "ingest", "--text", "x", "--text-ref", "t")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_no_wait_fails_fast(self):
        holder = _LockHolder(self.root)
        try:
            t0 = time.monotonic()
            p = _run(self.root, "--no-wait",
                     "ingest", "--text", "x", "--text-ref", "t")
            dt = time.monotonic() - t0
            self.assertEqual(p.returncode, 5, p.stdout + p.stderr)
            self.assertLess(dt, 5)
        finally:
            holder.release()

    def test_readonly_commands_do_not_wait(self):
        holder = _LockHolder(self.root)
        try:
            for args in (("status",), ("review", "triage"),
                         ("review", "list"),
                         ("ingest", "--list-connectors")):
                p = _run(self.root, "--wait-lock", "0.3", *args)
                self.assertEqual(p.returncode, 0,
                                 "%r -> %d: %s" % (args, p.returncode,
                                                   p.stdout + p.stderr))
        finally:
            holder.release()


if __name__ == "__main__":
    unittest.main()
