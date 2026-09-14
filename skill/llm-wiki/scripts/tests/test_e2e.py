import json, math, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, compile as wc_compile, contextpack, deps, pages, query, sources
from wikicore.store import Wiki, init_wiki

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import wiki as cli  # noqa: E402

ADR = """# ADR-019: Analytics attribution

Decided: production attribution uses a 30-day click lookback window.
Effective 2026-01-01.
"""

MEMO = """# Memo: attribution window change (2026-09-01)

Production click lookback window changed from 30 days to 7 days,
effective 2026-09-01. Sandbox remains 30 days.
"""


class TestEndToEnd(unittest.TestCase):
    def test_full_correction_flow(self):
        root = tempfile.mkdtemp()
        wiki = Wiki(root)
        self.assertEqual(cli.main(["--root", root, "init", "--name", "shop"]), 0)

        # ingest via CLI
        tmp = tempfile.mkdtemp()
        adr_path = os.path.join(tmp, "adr-019.md")
        memo_path = os.path.join(tmp, "memo.md")
        with open(adr_path, "w") as f:
            f.write(ADR)
        with open(memo_path, "w") as f:
            f.write(MEMO)
        self.assertEqual(cli.main(["--root", root, "ingest", "--file", adr_path]), 0)
        self.assertEqual(cli.main(["--root", root, "ingest", "--file", memo_path]), 0)

        plan = wc_compile.compile_plan(wiki)
        self.assertEqual(len(plan["pending"]), 2)

        # --- compile source 1 (ADR): 30-day claim -------------------------
        pending = {p["origin"]: p for p in plan["pending"]}
        self._compile_source(wiki, wiki, adr_path, "analytics", "click_lookback_window",
                             30, {"environment": "production"}, valid_from="2026-01-01",
                             rel="UNRELATED", target=None, valid_from_cls="2026-01-01")
        old = claims.list_claims(wiki)[0]

        # a derived page depends on the old claim
        txn = pages.Transaction(wiki, "concept") if hasattr(pages, "Transaction") else None
        from wikicore.transaction import Transaction as T
        txn = T(wiki, "concept")
        txn.stage_write("wiki/concepts/attribution.md",
                        "---\ntype: concept\ndeps:\n  - %s@%d\n---\n\n# Attribution\n\nWindow is [[%s]].\n"
                        % (old["id"], old["version"], old["id"]))
        from wikicore import deps as deps_mod
        deps_mod.register(txn, "wiki/concepts/attribution.md",
                          ["%s@%d" % (old["id"], old["version"])])
        txn.commit(wiki.revision())

        # --- compile source 2 (memo): correction to 7 days -----------------
        self._compile_source(wiki, wiki, memo_path, "analytics", "click_lookback_window",
                             7, {"environment": "production"}, valid_from="2026-09-01",
                             rel="CORRECTION", target=old)

        new = [c for c in claims.list_claims(wiki) if c["value"] == 7][0]
        old_after = claims.load_claim(wiki, old["id"])
        self.assertEqual(old_after["status"], "superseded")
        self.assertEqual(new["status"], "accepted")
        # gate: correction invalidated dependent artifacts
        self.assertTrue(deps_mod.is_stale(wiki, "wiki/concepts/attribution.md"))

        # rebuild derived artifacts
        pages.build_pages(wiki, wiki.revision())
        txn = T(wiki, "concept2")
        txn.stage_write("wiki/concepts/attribution.md",
                        "---\ntype: concept\ndeps:\n  - %s@%d\n---\n\n# Attribution\n\nWindow is [[%s]] (7 days).\n"
                        % (new["id"], new["version"], new["id"]))
        deps_mod.clear_stale(txn, ["wiki/concepts/attribution.md"])
        txn.commit(wiki.revision())
        v = pages.verify(wiki)
        self.assertTrue(v["ok"], v["errors"])

        # query: current answer is 7, historical is 30
        out = json.loads(self._capture_cli(["--root", root, "query-prepare",
                                            "--question", "what is the click lookback window?"]))
        current_values = [m["value"] for m in out["matches"]]
        self.assertIn(7, current_values)
        self.assertNotIn(30, current_values)
        hist = json.loads(self._capture_cli(["--root", root, "query-prepare", "--question",
                                             "what was the window in August?",
                                             "--as-of", "2026-08-15"]))
        self.assertIn(30, [m["value"] for m in hist["matches"]])

        # context pack with budget + receipt
        out = json.loads(self._capture_cli(["--root", root, "context-pack", "--task",
                                            "implement attribution fix", "--budget", "4000"]))
        self.assertLessEqual(out["receipt"]["budget"]["estimated"], 4000)

        # doctor clean of errors
        doc = json.loads(self._capture_cli(["--root", root, "doctor"]))
        self.assertTrue(doc["ok"], doc["findings"])

    def _compile_source(self, wiki, _unused, path, subject, predicate, value,
                        scope, valid_from, rel, target, valid_from_cls=None):
        pending = {p["origin"]: p for p in wc_compile.compile_plan(wiki)["pending"]}
        entry = pending[os.path.abspath(path)]
        cand = {
            "id": "auto", "subject": subject, "predicate": predicate, "value": value,
            "scope": scope, "valid_from": valid_from, "valid_to": None,
            "recorded_at": "2026-09-15T00:00:00Z", "status": "candidate",
            "proposed_by": "agent", "authority": {"type": "explicit_project_decision"},
            "evidence": [{"source_id": entry["source_id"],
                          "source_version": entry["version"],
                          "locator": {"type": "heading", "value": "H"}}],
            "supersedes": [],
        }
        run_id = wc_compile.stage_candidates(wiki, entry["source_id"], entry["version"],
                                             [cand], {
                                                 "source_id": entry["source_id"],
                                                 "source_version": entry["version"],
                                                 "coverage": {"text": "complete"},
                                                 "warnings": [], "parser": {"name": "x"}})
        wc_compile.reconcile_apply(wiki, run_id,
                                   [{"index": 0, "relationship": rel,
                                     "target_claim_id": target["id"] if target else None,
                                     "valid_from": valid_from_cls, "valid_to": None,
                                     "authority": {"type": "explicit_project_decision"}}],
                                   wiki.revision())

    def _capture_cli(self, argv):
        import io
        from contextlib import redirect_stdout

        buf = io.StringIO()
        with redirect_stdout(buf):
            code = cli.main(argv)
        self.assertEqual(code, 0, buf.getvalue())
        return buf.getvalue()


if __name__ == "__main__":
    unittest.main()
