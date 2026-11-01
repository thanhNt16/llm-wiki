"""SP3-T3 (#20): stage/run handshake — candidates reach the run that
asked for them.

Bug (repro on pre-fix commit):
    compile_umbrella mints run R1 whose pending.json names a
    candidates_path; stage_candidates(...) ALWAYS mints a second run R2
    (Transaction staging dir) holding compile.json; compile_umbrella
    (resume=True) only enumerates runs with pending.json, so it errors on
    R1's missing candidates.jsonl and never sees R2's staged candidates:

        resume raised: TxnError run <R1>: expected
        .../staging/<R1>/candidates.jsonl (write shorthand candidates there)

Fix contract:
- stage_candidates(..., run_id=R1) stages INTO R1's dir (compile.json +
  candidates.jsonl so `build`'s unstaged check sees it) — one run total.
- Resume picks up runs whose staged candidates would otherwise orphan:
  a run dir holding compile.json but no pending.json contributes its
  candidates to the resume enumeration instead of being skipped.
- Phase-1 enumeration prefers a run that already holds staged candidates
  for a pending (source_id, source_version) over minting a new dir.
"""
import json
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
sys.path.insert(0, os.path.join(REPO, "skill", "llm-wiki", "scripts"))

from wikicore import claims  # noqa: E402,F401 -- first: breaks the
# transaction<->ftx<->claims import cycle
from wikicore import compile as wc_compile, ids, sources  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import TxnError  # noqa: E402


def fresh():
    wiki = Wiki(tempfile.mkdtemp())
    init_wiki(wiki, "handshake-test")
    return wiki


def ingest(wiki, ref, body):
    return sources.ingest(wiki, "text", ref, body.encode("utf-8"))


REPORT = {"coverage": {"text": "complete"}, "warnings": [], "parser": {}}


def cand(subject, predicate, value):
    return {"subject": subject, "predicate": predicate, "value": value,
            "locator": "s:fixture", "authority": "doc"}


def staging_dirs(wiki):
    base = wiki.p(".state", "staging")
    if not os.path.isdir(base):
        return []
    return sorted(d for d in os.listdir(base)
                  if os.path.isdir(os.path.join(base, d)))


class TestStageIntoNamedRun(unittest.TestCase):
    def test_stage_into_named_run(self):
        wiki = fresh()
        ingest(wiki, "doc", "fact: window is 7 days")
        plan = wc_compile.compile_umbrella(wiki, resume=False)
        p = plan["pending"][0]
        r1 = p["run_id"]

        # hand the pending run's candidates back through stage_candidates
        # targeting R1
        staged = wc_compile.stage_candidates(
            wiki, p["source_id"], p["source_version"],
            [cand("project.window", "days", 7)], REPORT, run_id=r1)
        self.assertEqual(staged, r1)
        self.assertTrue(os.path.isfile(p["candidates_path"]),
                        "candidates.jsonl must land in the named run")
        self.assertEqual(staging_dirs(wiki), [r1],
                         "staging into R1 must not mint a second run dir")

        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out["phase"], "done")
        self.assertEqual(out["auto"]["unrelated"], 1)
        self.assertEqual(out["changes"]["claims_created"], 1)
        self.assertEqual(staging_dirs(wiki), [r1])
        self.assertEqual(
            [c["subject"] for c in claims.list_claims(wiki)],
            ["project.window"])

    def test_stage_unknown_run_id_rejected(self):
        wiki = fresh()
        r = ingest(wiki, "doc", "x")
        with self.assertRaises(TxnError):
            wc_compile.stage_candidates(
                wiki, r["source_id"], r["version"],
                [cand("s", "p", 1)], REPORT,
                run_id="run_0000000000000000000000000A")


class TestOrphanRunPickup(unittest.TestCase):
    def test_resume_picks_up_orphan_compile_run(self):
        # pre-T10 CLI stages candidates without a run_id — the run dir has
        # compile.json but no pending.json; resume must not orphan it
        wiki = fresh()
        r = ingest(wiki, "doc", "fact")
        rid = wc_compile.stage_candidates(
            wiki, r["source_id"], r["version"],
            [cand("project.window", "days", 7)], REPORT)
        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out["phase"], "done", out)
        self.assertEqual(out["changes"]["claims_created"], 1)
        self.assertEqual(
            [c["subject"] for c in claims.list_claims(wiki)],
            ["project.window"])
        self.assertEqual(staging_dirs(wiki), [rid])

    def test_phase1_prefers_run_with_staged_candidates(self):
        # a compile.json orphan for a pending version is adopted instead of
        # a second run dir being minted for the same source version
        wiki = fresh()
        ingest(wiki, "doc", "fact")
        plan = wc_compile.compile_umbrella(wiki, resume=False)
        sid = plan["pending"][0]["source_id"]
        ver = plan["pending"][0]["source_version"]
        orphan = wc_compile.stage_candidates(
            wiki, sid, ver, [cand("project.window", "days", 7)], REPORT)
        self.assertEqual(len(staging_dirs(wiki)), 2)  # pending + orphan

        plan2 = wc_compile.compile_umbrella(wiki, resume=False)
        self.assertEqual(len(plan2["pending"]), 1)
        self.assertEqual(plan2["pending"][0]["run_id"], orphan)
        self.assertTrue(
            os.path.isfile(plan2["pending"][0]["candidates_path"]))
        self.assertEqual(staging_dirs(wiki), [orphan],
                         "empty pending run is superseded by the "
                         "candidates-bearing run")


class TestRestageGuard(unittest.TestCase):
    """SP3-F5 (T3-S1): re-staging a run_id must not reuse stale
    comparisons.json/classifications.json — reconcile_prepare must re-run
    against the NEW candidates or stale match indices misindex pairs."""

    def test_restage_invalidates_prepared_state(self):
        wiki = fresh()
        r1 = ingest(wiki, "s1", "fact: order service uses postgres")
        wc_compile.stage_candidates(
            wiki, r1["source_id"], r1["version"],
            [cand("order_service", "db", "postgres")], REPORT)
        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out["phase"], "done")
        self.assertEqual(
            [c["subject"] for c in claims.list_claims(wiki)],
            ["order_service"])

        r2 = ingest(wiki, "s2", "fact: retry count is 5")
        plan = wc_compile.compile_umbrella(wiki, resume=False)
        p = plan["pending"][0]
        r = p["run_id"]
        staged = wc_compile.stage_candidates(
            wiki, r2["source_id"], r2["version"],
            [cand("order_service", "db", "mysql")], REPORT, run_id=r)
        self.assertEqual(staged, r)
        wc_compile.reconcile_prepare(wiki, r)  # tier-match -> auto None
        # agent adjudicates the tier match BEFORE the new staging lands
        d = wiki.p(".state", "staging", r)
        with open(os.path.join(d, "classifications.json"), "w") as f:
            json.dump({"classifications": [
                {"index": 0, "relationship": "UNRELATED"}]}, f)

        # agent re-stages into the same run with a different candidate
        wc_compile.stage_candidates(
            wiki, r2["source_id"], r2["version"],
            [cand("retries.count", "retries", 5)], REPORT, run_id=r)
        d = wiki.p(".state", "staging", r)
        self.assertFalse(os.path.isfile(os.path.join(d, "comparisons.json")),
                         "re-stage must invalidate prepared comparisons")
        self.assertFalse(
            os.path.isfile(os.path.join(d, "classifications.json")),
            "re-stage must invalidate classifications keyed to old indices")

        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out["phase"], "done", out)
        # s1's already-applied run replays its receipt (auto unrelated=1);
        # the re-staged run must add ITS OWN fresh verdict, not the stale
        # classification keyed to the old candidates
        self.assertEqual(out["auto"]["unrelated"], 2)
        self.assertEqual(out["changes"]["claims_created"], 2)
        self.assertEqual(
            sorted(c["subject"] for c in claims.list_claims(wiki)),
            ["order_service", "retries.count"])

    def test_restage_into_applied_run_rejected(self):
        wiki = fresh()
        r = ingest(wiki, "doc", "fact: window is 7 days")
        rid = wc_compile.stage_candidates(
            wiki, r["source_id"], r["version"],
            [cand("project.window", "days", 7)], REPORT)
        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out["phase"], "done")
        self.assertTrue(os.path.isfile(
            wiki.p(".state", "staging", rid, "receipt.json")))
        with self.assertRaises(TxnError):
            wc_compile.stage_candidates(
                wiki, r["source_id"], r["version"],
                [cand("other", "p", 1)], REPORT, run_id=rid)


class TestResumeSkipsMalformedRun(unittest.TestCase):
    """SP3-F5 (T3-N2): one malformed orphan run must not abort the whole
    resume — it is skipped with a warning while good runs still apply."""

    def test_malformed_orphan_warns_and_good_run_applies(self):
        wiki = fresh()
        r = ingest(wiki, "doc", "fact: window is 7 days")
        good = wc_compile.stage_candidates(
            wiki, r["source_id"], r["version"],
            [cand("project.window", "days", 7)], REPORT)
        # orphan dir: a bare candidates.jsonl lacking evidence-stamped
        # source identity -> _adopt_orphan_run raises TxnError on it.
        # ULIDs share the ms timestamp so enumeration order vs the good
        # run is random — the assertions hold either way.
        bad = ids.new("run")
        bad_d = wiki.p(".state", "staging", bad)
        os.makedirs(bad_d)
        with open(os.path.join(bad_d, "candidates.jsonl"), "w") as f:
            f.write(json.dumps({"subject": "x", "predicate": "y",
                                "value": 1}) + "\n")

        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(out["phase"], "done", out)
        self.assertEqual(out["changes"]["claims_created"], 1)
        self.assertEqual(
            [c["subject"] for c in claims.list_claims(wiki)],
            ["project.window"])
        self.assertTrue(
            any(bad in w for w in out.get("warnings", [])),
            "warnings must name the skipped run: %r" % (out,))

if __name__ == "__main__":
    unittest.main()
