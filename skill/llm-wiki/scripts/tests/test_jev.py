import argparse
import io
import json
import os
import sys
import unittest
import urllib.error
from contextlib import redirect_stdout
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from wikicore import jev, synthesize  # noqa: E402
from wikicore.transaction import TxnError  # noqa: E402
import wiki as wiki_mod  # noqa: E402
from test_synthesize import fresh, save_claim, page_md  # noqa: E402

ENV_KEYS = ("DECISION_TOKEN", "DECISION_MODEL", "DECISION_ENDPOINT")
FAKE_ENV = {"DECISION_TOKEN": "sk-test", "DECISION_MODEL": "m/test"}


def _wire(answers, usage=None):
    return {"success": True,
            "result": {"model": "m/test", "answers": answers,
                       "usage": usage or {"tokens": 1}}}




class JevClientTest(unittest.TestCase):
    def setUp(self):
        self._env = mock.patch.dict(os.environ, {})
        self._env.start()
        for k in ENV_KEYS:
            os.environ.pop(k, None)

    def tearDown(self):
        self._env.stop()

    def test_disabled_without_env(self):
        self.assertFalse(jev.is_ready())
        with mock.patch.object(jev, "_post") as post:
            r = jev.ask("s", {"q": {"type": "noul", "instructions": "?"}})
        self.assertFalse(r["ok"])
        self.assertTrue(r["disabled"])
        self.assertEqual(r["answers"], {})
        self.assertIsNone(r["error"])
        post.assert_not_called()

    def test_missing_one_var_disables(self):
        os.environ["DECISION_TOKEN"] = "sk-test"
        self.assertFalse(jev.is_ready())
        r = jev.ask("s", {})
        self.assertFalse(r["ok"])
        self.assertTrue(r["disabled"])

    def test_endpoint_default_and_override(self):
        self.assertEqual(jev.endpoint(),
                         "http://localhost:20128/v1/systemone")
        os.environ["DECISION_ENDPOINT"] = "http://x:1/v2"
        self.assertEqual(jev.endpoint(), "http://x:1/v2")

    def test_ask_normalizes_noul(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post",
                                   return_value=_wire(
                                       {"q": {"type": "noul",
                                              "noul": 0.9}})) as post:
                r = jev.ask("state text",
                            {"q": {"type": "noul", "instructions": "?"}})
        self.assertTrue(r["ok"])
        self.assertFalse(r["disabled"])
        self.assertIsNone(r["error"])
        self.assertEqual(r["answers"]["q"], {"type": "noul", "value": 0.9})
        self.assertEqual(r["usage"], {"tokens": 1})
        payload = post.call_args[0][0]
        self.assertEqual(payload["model"], "m/test")
        self.assertEqual(payload["state"], "state text")
        self.assertIn("q", payload["questions"])

    def test_ask_normalizes_choice(self):
        # official wire: "choice" is the picked string + probabilities map
        wire = {"kind": {"type": "choice", "choice": "entities",
                         "confidence": 0.62,
                         "probabilities": {"concepts": 0.1,
                                           "entities": 0.8,
                                           "procedures": 0.05,
                                           "questions": 0.05}}}
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post", return_value=_wire(wire)):
                r = jev.ask("s", {"kind": {"type": "choice",
                                          "options": ["concepts"]}})
        v = r["answers"]["kind"]["value"]
        self.assertEqual(v["pick"], "entities")
        self.assertEqual(v["distribution"]["entities"], 0.8)
        self.assertEqual(v["confidence"], 0.62)

    def test_ask_normalizes_choice_nested_shape(self):
        wire = {"kind": {"type": "choice",
                         "choice": {"pick": "entities",
                                    "probs": {"concepts": 0.2,
                                              "entities": 0.8}}}}
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post", return_value=_wire(wire)):
                r = jev.ask("s", {"kind": {"type": "choice",
                                          "options": ["concepts"]}})
        v = r["answers"]["kind"]["value"]
        self.assertEqual(v["pick"], "entities")
        self.assertEqual(v["distribution"]["entities"], 0.8)

    def test_ask_normalizes_score(self):
        # official wire: "score" is the expected level + probabilities
        wire = {"sev": {"type": "score", "score": 1.0, "confidence": 1.0,
                        "probabilities": {"0": 0.0, "1": 1.0, "2": 0.0}}}
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post", return_value=_wire(wire)):
                r = jev.ask("s", {"sev": {"type": "score",
                                         "levels": 3}})
        self.assertEqual(r["answers"]["sev"],
                         {"type": "score",
                          "value": {"level": 1, "expected": 1.0}})

    def test_to_wire_options_become_criteria(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(
                    jev, "_post",
                    return_value=_wire({"k": {"type": "choice",
                                              "choice": "b",
                                              "probabilities": {"b": 1.0,
                                                                "a": 0.0}}})) as post:
                jev.ask("s", {"k": {"type": "choice",
                                    "options": ["a", "b"],
                                    "instructions": "pick"}})
        q = post.call_args[0][0]["questions"]["k"]
        self.assertEqual(q["criteria"], {"a": "a", "b": "b"})
        self.assertNotIn("options", q)

    def test_transport_error_returns_ok_false(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(
                    jev, "_post",
                    side_effect=urllib.error.URLError("refused")):
                r = jev.ask("s", {"q": {"type": "noul"}})
        self.assertFalse(r["ok"])
        self.assertFalse(r["disabled"])
        self.assertIn("refused", r["error"])

    def test_timeout_returns_ok_false(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post",
                                   side_effect=TimeoutError("timed out")):
                r = jev.ask("s", {"q": {"type": "noul"}})
        self.assertFalse(r["ok"])
        self.assertIn("timed out", r["error"])

    def test_success_false_returns_ok_false(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(
                    jev, "_post",
                    return_value={"success": False, "error": "bad model"}):
                r = jev.ask("s", {"q": {"type": "noul"}})
        self.assertFalse(r["ok"])
        self.assertEqual(r["error"], "bad model")


class JevCliTest(unittest.TestCase):
    def setUp(self):
        self._env = mock.patch.dict(os.environ, {})
        self._env.start()
        for k in ENV_KEYS:
            os.environ.pop(k, None)

    def tearDown(self):
        self._env.stop()

    def _main(self, argv):
        out = io.StringIO()
        with redirect_stdout(out):
            code = wiki_mod.main(argv)
        return code, json.loads(out.getvalue())

    def test_check_ready_exit0(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            code, payload = self._main(["jev", "--check"])
        self.assertEqual(code, 0)
        self.assertTrue(payload["ready"])
        self.assertEqual(payload["model"], "m/test")
        self.assertEqual(payload["endpoint"],
                         "http://localhost:20128/v1/systemone")

    def test_check_not_ready_exit0(self):
        code, payload = self._main(["jev", "--check"])
        self.assertEqual(code, 0)
        self.assertFalse(payload["ready"])
        self.assertEqual(payload["reason"],
                         "DECISION_TOKEN and DECISION_MODEL unset")

    def test_check_partial_env_names_missing(self):
        os.environ["DECISION_TOKEN"] = "sk-test"
        code, payload = self._main(["jev", "--check"])
        self.assertEqual(code, 0)
        self.assertEqual(payload["reason"], "DECISION_MODEL unset")

    def test_query_prints_ask_result(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(
                    jev, "_post",
                    return_value=_wire({"q": {"type": "noul",
                                              "noul": 0.25}})):
                code, payload = self._main(
                    ["jev", "--state", "x",
                     "--question", '{"q": {"type": "noul", '
                                   '"instructions": "same?"}}'])
        self.assertEqual(code, 0)
        self.assertTrue(payload["ok"])
        self.assertEqual(payload["answers"]["q"],
                         {"type": "noul", "value": 0.25})

    def test_query_disabled_without_env(self):
        code, payload = self._main(
            ["jev", "--question", '{"q": {"type": "noul"}}'])
        self.assertEqual(code, 0)
        self.assertFalse(payload["ok"])
        self.assertTrue(payload["disabled"])

    def test_query_bad_json_exit3(self):
        code, payload = self._main(["jev", "--question", "{nope"])
        self.assertEqual(code, 3)
        self.assertIn("not valid JSON", payload["error"])


def _targets_args(wiki, **kw):
    base = dict(root=wiki.root, kind=None, stale_only=False,
                min_claims=2, min_sources=2, decide=False)
    base.update(kw)
    return argparse.Namespace(**base)


def _run_page_targets(ns):
    out = io.StringIO()
    with redirect_stdout(out):
        wiki_mod.cmd_page_targets(ns)
    return json.loads(out.getvalue())


def _write_args(wiki, md, artifact, **kw):
    base = dict(root=wiki.root, file=None, text=md, artifact=artifact,
                deps_file=None, deps=None, allow_new=False,
                base_revision=None, decide=False)
    base.update(kw)
    return argparse.Namespace(**base)


def _plant_page(wiki, kind, slug, subject, title):
    rel = "wiki/%s/%s.md" % (kind, slug)
    path = wiki.p(rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write("---\ntype: entity\nsubject: %s\ntitle: %s\n---\n\nbody\n"
                % (subject, title))
    return rel


class PageTargetsDecideTest(unittest.TestCase):
    def setUp(self):
        self._env = mock.patch.dict(os.environ, {})
        self._env.start()
        for k in ENV_KEYS:
            os.environ.pop(k, None)
        self.wiki = fresh()
        # "stc-88" -> heuristic:2 (entities); "billing overview" -> default
        save_claim(self.wiki, "stc-88", predicate="status", value="shipped",
                   root_origin="origin-1")
        save_claim(self.wiki, "stc-88", predicate="root cause", value="x",
                   root_origin="origin-2")
        save_claim(self.wiki, "billing overview",
                   authority={"type": "explicit_project_decision"})

    def tearDown(self):
        self._env.stop()

    def test_decide_off_identical_output(self):
        a = _run_page_targets(_targets_args(self.wiki, decide=False))
        b = _run_page_targets(_targets_args(self.wiki, decide=True))
        self.assertEqual(a, b)
        for t in b["targets"]:
            self.assertNotIn("decision", t)

    def test_decide_attaches_verdict_to_heuristic_targets(self):
        wire = {"kind": {"type": "choice",
                         "choice": {"pick": "entities",
                                    "probs": {"entities": 0.9,
                                              "concepts": 0.1}}}}
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post",
                                   return_value=_wire(wire)) as post:
                out = _run_page_targets(
                    _targets_args(self.wiki, decide=True))
        rows = {t["subject"]: t for t in out["targets"]}
        self.assertEqual(rows["stc-88"]["decision"],
                         {"suggested_kind": "entities", "confidence": 0.9})
        self.assertNotIn("decision", rows["billing overview"])
        # exactly one ask for the heuristic-routed target only
        self.assertEqual(post.call_count, 1)
        payload = post.call_args[0][0]
        self.assertIn("stc-88", payload["state"])
        q = payload["questions"]["kind"]
        self.assertEqual(q["type"], "choice")
        self.assertEqual(sorted(q["criteria"]),
                         ["concepts", "entities", "procedures", "questions"])

    def test_decide_endpoint_down_no_decision_fields(self):
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(
                    jev, "_post",
                    side_effect=urllib.error.URLError("down")):
                out = _run_page_targets(
                    _targets_args(self.wiki, decide=True))
        for t in out["targets"]:
            self.assertNotIn("decision", t)


class WritePageDecideTest(unittest.TestCase):
    def setUp(self):
        self._env = mock.patch.dict(os.environ, {})
        self._env.start()
        for k in ENV_KEYS:
            os.environ.pop(k, None)
        self.wiki = fresh()
        # existing page: tokens {billing, service}
        _plant_page(self.wiki, "entities", "billing-service",
                    "billing service", "Billing Service")
        c = save_claim(self.wiki, "billing service api", predicate="status",
                       value="ok", root_origin="o1")
        self.dep = "%s@1" % c["id"]
        # new page slug differs but shares 2/3 tokens -> Jaccard 0.667 >=
        # NEAR_DUP_WRITE_THRESHOLD -> near-duplicate refusal
        self.artifact = "wiki/entities/billing-service-api.md"
        self.md = page_md(
            [self.dep],
            body="Billing service api notes [[%s]]." % c["id"],
            type_="entity", subject="billing service api").replace(
                "title: DB engine", "title: Billing Service API")

    def tearDown(self):
        self._env.stop()

    def _run_main(self, argv):
        out = io.StringIO()
        with redirect_stdout(out):
            code = wiki_mod.main(argv)
        return code, json.loads(out.getvalue())

    def test_guard_refuses(self):
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(
                self.wiki, self.artifact, self.md, [self.dep],
                self.wiki.revision())
        self.assertTrue(str(ctx.exception).startswith("near-duplicate page"))

    def test_refusal_identical_without_env(self):
        code, payload = self._run_main([
            "--root", self.wiki.root, "write-page",
            "--artifact", self.artifact,
            "--text", self.md, "--deps", self.dep, "--decide"])
        self.assertEqual(code, 3)
        self.assertTrue(payload["error"].startswith("near-duplicate page"))
        self.assertNotIn("decision", payload)

    def test_decide_verdict_lands_in_error_json(self):
        wire = {"same_page": {"type": "noul", "noul": 0.87}}
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post",
                                   return_value=_wire(wire)) as post:
                code, payload = self._run_main([
                    "--root", self.wiki.root, "write-page",
                    "--artifact", self.artifact,
                    "--text", self.md, "--deps", self.dep, "--decide"])
        self.assertEqual(code, 3)
        self.assertTrue(payload["error"].startswith("near-duplicate page"))
        self.assertEqual(payload["decision"], {"same_page_p": 0.87})
        payload_sent = post.call_args[0][0]
        self.assertIn("billing service api", payload_sent["state"])
        self.assertEqual(payload_sent["questions"]["same_page"]["type"],
                         "noul")

    def test_decide_not_attached_to_other_errors(self):
        # a non-near-dup TxnError (deps required) must not get a decision
        md = page_md(None, body="no links", type_="entity",
                     subject="billing service")
        with mock.patch.dict(os.environ, FAKE_ENV):
            with mock.patch.object(jev, "_post") as post:
                code, payload = self._run_main([
                    "--root", self.wiki.root, "write-page",
                    "--artifact", "wiki/entities/billing-service.md",
                    "--text", md, "--decide"])
        self.assertEqual(code, 3)
        self.assertNotIn("decision", payload)
        post.assert_not_called()


if __name__ == "__main__":
    unittest.main()
