import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, sources, synthesize, topics
from wikicore import compile as wc_compile
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "topics-test")
    return wiki


def _commit(wiki, fn):
    txn = Transaction(wiki, "test")
    fn(txn)
    return txn.commit(wiki.revision())


class TestTopicRows(unittest.TestCase):
    def test_parse_rows_validates(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "topics.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"lake.s3","kind":"entity","title":"S3"}\n')
            f.write('{"subject":"lake.dl","kind":"concepts","title":"Data lake"}\n')
        rows, kinds = topics.parse_topics_file(p)
        self.assertEqual(rows[0]["kind"], "entity")
        self.assertEqual(rows[1]["kind"], "concept")  # plural normalized

    def test_parse_rows_rejects_bad_kind(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "t.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a.b","kind":"bogus","title":"T"}\n')
        with self.assertRaises(TxnError):
            topics.parse_topics_file(p)

    def test_parse_rows_rejects_missing_field_with_line(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "t.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a.b","title":"no kind"}\n')
        with self.assertRaises(TxnError) as cm:
            topics.parse_topics_file(p)
        self.assertIn("line 1", str(cm.exception))


class TestMerge(unittest.TestCase):
    def test_same_kind_merges_aliases_first_title_wins(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3", "aliases": ["AWS S3"]},
            {"subject": "lake.s3", "kind": "entity", "title": "Amazon S3", "aliases": ["S3"]},
        ], {"source_id": "src_a", "version": 1}))
        e = topics.get(wiki, "lake.s3")
        self.assertEqual(e["title"], "S3")
        self.assertEqual(sorted(e["aliases"]), ["AWS S3", "S3"])

    def test_kind_conflict_keeps_first_kind(self):
        wiki = fresh()
        out = {}
        def go(t):
            out.update(topics.merge_rows(t, [
                {"subject": "lake.delta", "kind": "entity", "title": "Delta"},
                {"subject": "lake.delta", "kind": "concept", "title": "Delta"},
            ], {"source_id": "src_a", "version": 1}))
        _commit(wiki, go)
        e = topics.get(wiki, "lake.delta")
        self.assertEqual(e["kind"], "entity")
        self.assertEqual(e["status"], "conflict")
        self.assertEqual(e["alternate_kinds"], ["concept"])
        self.assertEqual(out["kind_conflicts"], ["lake.delta"])

    def test_auto_register_creates_concept(self):
        wiki = fresh()
        out = {}
        def go(t):
            out.update(topics.auto_register(t, ["misc.thing"],
                                            {"source_id": "src_a", "version": 1}))
        _commit(wiki, go)
        e = topics.get(wiki, "misc.thing")
        self.assertEqual(e["kind"], "concept")
        self.assertTrue(e["auto_registered"])
        self.assertEqual(out["auto_registered"], ["misc.thing"])

    def test_decision_kind_never_conflicts_with_merge(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "policy.pg", "kind": "decision", "title": "Use PG"},
        ], {"source_id": "src_a", "version": 1}))
        self.assertEqual(topics.get(wiki, "policy.pg")["kind"], "decision")

    def test_two_conflicts_in_one_txn_both_reach_review_queue(self):
        """Regression: review.add_item must compose over the txn's staged
        queue — a second conflict in the same merge must not drop the first."""
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.delta", "kind": "entity", "title": "Delta"},
            {"subject": "lake.delta", "kind": "concept", "title": "Delta"},
            {"subject": "lake.ice", "kind": "entity", "title": "Iceberg"},
            {"subject": "lake.ice", "kind": "procedure", "title": "Iceberg"},
        ], {"source_id": "src_a", "version": 1}))
        with open(wiki.p(".state", "review-queue.jsonl")) as f:
            items = [json.loads(l) for l in f if l.strip()]
        conflicts = [i for i in items if i["kind"] == "topic_kind_conflict"]
        self.assertEqual(len(conflicts), 2)
        self.assertEqual(sorted(i["affected"][0] for i in conflicts),
                         ["lake.delta", "lake.ice"])


class TestRegistryFile(unittest.TestCase):
    def test_registry_json_shape_and_version(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3"},
        ], {"source_id": "src_a", "version": 1}))
        reg = topics.load(wiki)
        self.assertEqual(reg["version"], 1)
        self.assertIn("lake.s3", reg["topics"])
        self.assertEqual(reg["topics"]["lake.s3"]["declared_by"],
                         [{"source_id": "src_a", "version": 1}])


class TestCompileTopics(unittest.TestCase):
    def _run_umbrella(self, wiki, topics_rows=None, cands=None):
        """Full umbrella: compile -> write files -> resume."""
        r = sources.ingest(wiki, "text", "doc-t", b"# Doc\nS3 is object storage.")
        plan = wc_compile.compile_umbrella(wiki)
        meta = plan["pending"][0]
        if topics_rows is not None:
            with open(meta["topics_path"], "w") as f:
                for row in topics_rows:
                    f.write(json.dumps(row) + "\n")
        with open(meta["candidates_path"], "w") as f:
            for c in (cands or []):
                f.write(json.dumps(c) + "\n")
        res = wc_compile.compile_umbrella(wiki, resume=True)
        while res["phase"] == "classify":
            cls = [{"index": row["index"], "relationship": "CONTRADICTION",
                    "target_claim_id": None, "authority": {"type": "authoritative_source"}}
                   for row in res["needs_review"]]
            with open(meta["classifications_path"], "w") as f:
                json.dump(cls, f)
            res = wc_compile.compile_umbrella(wiki, resume=True)
        return r, res

    def test_pending_entry_has_topics_path(self):
        wiki = fresh()
        sources.ingest(wiki, "text", "doc-t", b"hello")
        plan = wc_compile.compile_umbrella(wiki)
        self.assertIn("topics_path", plan["pending"][0])
        self.assertTrue(plan["pending"][0]["topics_path"].endswith("topics.jsonl"))

    def test_declared_topics_registered_and_in_receipt(self):
        wiki = fresh()
        cand = {"subject": "lake.s3", "predicate": "type", "value": "object store",
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "lake.s3", "kind": "entity", "title": "S3"}],
            cands=[cand])
        t = topics.get(wiki, "lake.s3")
        self.assertEqual(t["kind"], "entity")
        self.assertEqual(res["topics"]["registered"], 1)

    def test_source_topic_auto_registered(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki, topics_rows=[], cands=[])
        reg = topics.load(wiki)["topics"]
        self.assertTrue(any(e["kind"] == "source" for e in reg.values()))

    def test_undeclared_subject_auto_registered_with_warning(self):
        wiki = fresh()
        cand = {"subject": "misc.thing", "predicate": "p", "value": 1,
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki, topics_rows=[], cands=[cand])
        e = topics.get(wiki, "misc.thing")
        self.assertTrue(e["auto_registered"])
        self.assertIn("misc.thing", res["topics"]["auto_registered"])

    def test_missing_topics_file_warns_not_fails(self):
        wiki = fresh()
        cand = {"subject": "x.y", "predicate": "p", "value": 1,
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki, topics_rows=None, cands=[cand])
        self.assertEqual(res["phase"], "done")
        self.assertTrue(any("no_topics_declared" in w
                            for w in res["topics"]["warnings"]))

    def test_unclaimed_topic_listed(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "lake.presto", "kind": "entity", "title": "P"}],
            cands=[])
        self.assertIn("lake.presto", res["topics"]["unclaimed"])

    def test_decision_topic_in_receipt_not_page(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "policy.pg", "kind": "decision", "title": "PG"}],
            cands=[])
        self.assertIn("policy.pg", res["topics"]["decision_topics"])
        self.assertEqual(topics.get(wiki, "policy.pg")["kind"], "decision")


    def test_unclaimed_aggregate_uses_final_registry(self):
        """Run A declares topic X (no claims); run B claims X — aggregate
        unclaimed must NOT contain X even though run A's per-run snapshot did."""
        wiki = fresh()
        ra = sources.ingest(wiki, "text", "doc-a", b"# A\nDeclares X.")
        rb = sources.ingest(wiki, "text", "doc-b", b"# B\nClaims X.")
        plan = wc_compile.compile_umbrella(wiki)
        self.assertEqual(len(plan["pending"]), 2)
        for meta in plan["pending"]:
            if meta["source_id"] == ra["source_id"]:
                with open(meta["topics_path"], "w") as f:
                    f.write(json.dumps(
                        {"subject": "misc.x", "kind": "concept", "title": "X"}) + "\n")
                with open(meta["candidates_path"], "w") as f:
                    pass
            else:
                with open(meta["candidates_path"], "w") as f:
                    f.write(json.dumps(
                        {"subject": "misc.x", "predicate": "p", "value": 1,
                         "locator": "h:B", "authority": "doc"}) + "\n")
        res = wc_compile.compile_umbrella(wiki, resume=True)
        while res["phase"] == "classify":
            meta = next(m for m in plan["pending"]
                        if m["source_id"] == res["needs_review"][0]["source_id"])
            cls = [{"index": row["index"], "relationship": "CONTRADICTION",
                    "target_claim_id": None,
                    "authority": {"type": "authoritative_source"}}
                   for row in res["needs_review"]
                   if row["source_id"] == meta["source_id"]]
            with open(meta["classifications_path"], "w") as f:
                json.dump(cls, f)
            res = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertEqual(res["phase"], "done")
        self.assertNotIn("misc.x", res["topics"]["unclaimed"])

    def test_lowlevel_receipt_has_full_topics_shape(self):
        """reconcile_apply without declared_by still carries all 7 keys."""
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc-l", b"# L\nLow level.")
        cand = {"subject": "misc.l", "predicate": "p", "value": 1,
                "locator": "h:L",
                "scope": {}, "valid_from": None, "valid_to": None,
                "recorded_at": "2026-10-16T00:00:00Z", "status": "candidate",
                "proposed_by": "agent",
                "authority": {"type": "authoritative_source"},
                "evidence": [{"source_id": r["source_id"],
                              "source_version": r["version"],
                              "locator": {"type": "heading", "value": "L"}}],
                "supersedes": []}
        run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"],
                                             [cand], {"source_id": r["source_id"],
                                                      "source_version": r["version"],
                                                      "coverage": {"text": "complete"},
                                                      "warnings": [],
                                                      "parser": {"name": "t", "version": "1"}})
        wc_compile.reconcile_prepare(wiki, run_id)
        receipt = wc_compile.reconcile_apply(wiki, run_id, [], wiki.revision())
        self.assertEqual(receipt["topics"]["unclaimed"], [])
        self.assertEqual(set(receipt["topics"]),
                         {"declared", "registered", "auto_registered",
                          "kind_conflicts", "decision_topics", "unclaimed",
                          "warnings"})



class TestPageTargets(unittest.TestCase):
    def _claim(self, wiki, r, subject):
        """Stage+apply one accepted claim via direct txn."""
        cand = {"id": "auto", "subject": subject, "predicate": "p", "value": 1,
                "scope": {}, "valid_from": None, "valid_to": None,
                "recorded_at": "2026-10-01T00:00:00Z", "status": "candidate",
                "proposed_by": "agent",
                "authority": {"type": "authoritative_source", "source": "doc"},
                "evidence": [{"source_id": r["source_id"],
                              "source_version": r["version"],
                              "locator": {"type": "heading", "value": "H"}}],
                "supersedes": []}
        run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"],
                                             [cand], {"source_id": r["source_id"],
                                                      "source_version": r["version"],
                                                      "coverage": {"text": "complete"},
                                                      "warnings": [],
                                                      "parser": {"name": "t", "version": "1"}})
        wc_compile.reconcile_prepare(wiki, run_id)
        wc_compile.reconcile_apply(wiki, run_id,
            [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
              "valid_from": None, "valid_to": None,
              "authority": {"type": "authoritative_source"}}], wiki.revision())

    def test_declared_single_claim_target_with_declared_kind(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"S3 stores objects.")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        self._claim(wiki, r, "lake.s3")
        res = synthesize.page_targets(wiki)
        tg = [t for t in res["targets"] if t["subject"] == "lake.s3"]
        self.assertEqual(len(tg), 1)
        self.assertEqual(tg[0]["artifact"], "wiki/entities/lake-s3.md")
        self.assertEqual(tg[0]["reason"], "declared")

    def test_undeclared_single_claim_stays_waiting(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        self._claim(wiki, r, "misc.thing")  # auto-registered NOT applied here
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "misc.thing" for t in res["targets"]))
        self.assertTrue(any(w["subject"] == "misc.thing" for w in res["waiting"]))

    def test_conflict_subject_falls_back_to_rules(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.d", "kind": "entity", "title": "D"},
            {"subject": "lake.d", "kind": "concept", "title": "D"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        self._claim(wiki, r, "lake.d")
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "lake.d" for t in res["targets"]))
        self.assertTrue(any(w["subject"] == "lake.d" for w in res["waiting"]))

    def test_declared_decision_topic_never_targets(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "policy.pg", "kind": "decision", "title": "PG"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        self._claim(wiki, r, "policy.pg")
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "policy.pg" for t in res["targets"]))
        self.assertFalse(any(w["subject"] == "policy.pg" for w in res["waiting"]))

    def test_declared_topic_zero_claims_waiting(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.z", "kind": "entity", "title": "Z"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "lake.z" for t in res["targets"]))
        w = next((x for x in res["waiting"] if x["subject"] == "lake.z"), None)
        self.assertIsNotNone(w, res["waiting"])
        self.assertEqual(w["reason"], "declared_no_claims")

    def test_decision_topic_corroborated_still_no_target(self):
        """Resolved decision topic + claims meeting old thresholds: no target."""
        wiki = fresh()
        ra = sources.ingest(wiki, "text", "doc-a", b"a")
        rb = sources.ingest(wiki, "text", "doc-b", b"b")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "policy.two", "kind": "decision", "title": "Two"},
        ], {"source_id": ra["source_id"], "version": ra["version"]}))
        self._claim(wiki, ra, "policy.two")
        self._claim(wiki, rb, "policy.two")
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "policy.two" for t in res["targets"]))
        self.assertFalse(any(w["subject"] == "policy.two" for w in res["waiting"]))

    def test_source_topic_no_declared_no_claims_row(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.ensure_source_topic(
            t, "source.doc", "doc",
            {"source_id": r["source_id"], "version": r["version"]}))
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "source.doc" for t in res["targets"]))
        self.assertFalse(any(w["subject"] == "source.doc" for w in res["waiting"]))


class TestRecordDecision(unittest.TestCase):
    def _accepted_claim(self, wiki, r):
        cand = {"id": "auto", "subject": "policy.pg", "predicate": "db", "value": "postgres",
                "scope": {}, "valid_from": None, "valid_to": None,
                "recorded_at": "2026-10-01T00:00:00Z", "status": "candidate",
                "proposed_by": "agent",
                "authority": {"type": "explicit_project_decision", "source": "ADR"},
                "evidence": [{"source_id": r["source_id"], "source_version": r["version"],
                              "locator": {"type": "heading", "value": "H"}}],
                "supersedes": []}
        run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"], [cand],
            {"source_id": r["source_id"], "source_version": r["version"],
             "coverage": {"text": "complete"}, "warnings": [],
             "parser": {"name": "t", "version": "1"}})
        wc_compile.reconcile_prepare(wiki, run_id)
        wc_compile.reconcile_apply(wiki, run_id,
            [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
              "valid_from": None, "valid_to": None,
              "authority": {"type": "explicit_project_decision"}}], wiki.revision())
        return claims.list_claims(wiki)[0]["id"]

    def test_record_decision_happy_path(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "adr", b"use postgres")
        cid = self._accepted_claim(wiki, r)
        out = claims.create_decision(wiki, {
            "title": "Use Postgres", "status": "accepted",
            "decided_on": "2026-10-01", "claims": [cid],
            "evidence": [{"source_id": r["source_id"], "source_version": r["version"],
                          "locator": {"type": "heading", "value": "H"}}],
            "body": "ADR-001"})
        self.assertTrue(out["id"].startswith("decision_"))
        d = claims.load_decision(wiki, out["id"])
        self.assertEqual(d["claims"], [cid])
        self.assertEqual(d["version"], 1)

    def test_record_decision_rejects_unknown_claim(self):
        wiki = fresh()
        with self.assertRaises(TxnError) as cm:
            claims.create_decision(wiki, {
                "title": "X", "status": "accepted", "decided_on": "2026-10-01",
                "claims": ["claim_0000000000000000000000000A"], "evidence": []})
        self.assertIn("unknown claim", str(cm.exception))

    def test_record_decision_rejects_stamped_id(self):
        wiki = fresh()
        with self.assertRaises(TxnError):
            claims.create_decision(wiki, {
                "id": "decision_0000000000000000000000000A", "title": "X",
                "status": "accepted", "decided_on": "2026-10-01",
                "claims": [], "evidence": []})

    def test_record_decision_rejects_terminal_claim(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "adr", b"use postgres")
        cid = self._accepted_claim(wiki, r)
        c = claims.load_claim(wiki, cid)
        c["status"] = "rejected"
        c["acceptance"] = {"reason": "human_review"}
        _commit(wiki, lambda t: claims.save_claim(t, c))
        with self.assertRaises(TxnError) as cm:
            claims.create_decision(wiki, {
                "title": "X", "status": "accepted", "decided_on": "2026-10-01",
                "claims": [cid], "evidence": []})
        self.assertIn("rejected", str(cm.exception))

    def test_record_decision_rejects_unknown_source(self):
        wiki = fresh()
        with self.assertRaises(TxnError) as cm:
            claims.create_decision(wiki, {
                "title": "X", "status": "accepted", "decided_on": "2026-10-01",
                "claims": [],
                "evidence": [{"source_id": "src_0000000000000000000000000A",
                              "source_version": 1,
                              "locator": {"type": "heading", "value": "H"}}]})
        self.assertIn("unknown source", str(cm.exception))

    def test_record_decision_rejects_non_dict_evidence(self):
        wiki = fresh()
        with self.assertRaises(TxnError) as cm:
            claims.create_decision(wiki, {
                "title": "X", "status": "accepted", "decided_on": "2026-10-01",
                "claims": [], "evidence": ["not-a-dict"]})
        self.assertIn("evidence[0]", str(cm.exception))


class TestReviewAndDoctor(unittest.TestCase):
    def _conflict(self, wiki):
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.d", "kind": "entity", "title": "D"},
            {"subject": "lake.d", "kind": "concept", "title": "D"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        return [i for i in wiki.read_jsonl(".state/review-queue.jsonl")
                if i.get("kind") == "topic_kind_conflict"][0]

    def test_set_topic_kind_resolves_conflict(self):
        from wikicore import review
        wiki = fresh()
        item = self._conflict(wiki)
        receipt = review.act(wiki, item["id"], "set_topic_kind",
                             {"subject": "lake.d", "kind": "concept"},
                             wiki.revision())
        e = topics.get(wiki, "lake.d")
        self.assertEqual(e["kind"], "concept")
        self.assertEqual(e["status"], "resolved")
        self.assertEqual(receipt["item"]["status"], "resolved")

    def test_set_topic_kind_rejects_resolved_subject(self):
        from wikicore import review
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.e", "kind": "entity", "title": "E"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        # need a review item to act on — use the conflict path then re-resolve
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.e", "kind": "concept", "title": "E"},
        ], {"source_id": "src2", "version": 1}))
        item = [i for i in wiki.read_jsonl(".state/review-queue.jsonl")
                if i.get("kind") == "topic_kind_conflict"][0]
        review.act(wiki, item["id"], "set_topic_kind",
                   {"subject": "lake.e", "kind": "concept"}, wiki.revision())
        # acting again on same item now that status=resolved must fail
        with self.assertRaises(TxnError):
            review.act(wiki, item["id"], "set_topic_kind",
                       {"subject": "lake.e", "kind": "entity"}, wiki.revision())

    def test_doctor_flags_conflict_and_unclaimed(self):
        from wikicore import doctor
        wiki = fresh()
        self._conflict(wiki)
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.lonely", "kind": "entity", "title": "L"},
        ], {"source_id": "srcx", "version": 1}))
        res = doctor.run(wiki) if hasattr(doctor, "run") else doctor.doctor(wiki)
        codes = {f["code"] for f in res["findings"]}
        self.assertIn("topic_kind_conflict", codes)
        self.assertIn("topic_unclaimed", codes)

    def test_set_topic_kind_requires_conflict_item_and_affected(self):
        from wikicore import review
        wiki = fresh()
        self._conflict(wiki)  # lake.d conflict; item affected=[lake.d]
        # a non-conflict item may not be closed by set_topic_kind
        holder = {}
        def _add(t):
            holder["id"] = review.add_item(
                t, "ambiguous_scope", "scope unclear", [],
                ["lake.d"], "risk")
        _commit(wiki, _add)
        with self.assertRaises(TxnError):
            review.act(wiki, holder["id"], "set_topic_kind",
                       {"subject": "lake.d", "kind": "concept"},
                       wiki.revision())
        # a conflict item may not resolve a subject it does not affect
        item = [i for i in wiki.read_jsonl(".state/review-queue.jsonl")
                if i.get("kind") == "topic_kind_conflict"][0]
        with self.assertRaises(TxnError):
            review.act(wiki, item["id"], "set_topic_kind",
                       {"subject": "lake.other", "kind": "concept"},
                       wiki.revision())

    def test_doctor_flags_page_kind_mismatch(self):
        from wikicore import doctor
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.m", "kind": "entity", "title": "M"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        # simulate a page written under the wrong kind dir
        wdir = wiki.p("wiki", "concepts")
        os.makedirs(wdir, exist_ok=True)
        with open(os.path.join(wdir, topics.slugify("lake.m") + ".md"), "w") as f:
            f.write("stale page")
        res = doctor.run(wiki) if hasattr(doctor, "run") else doctor.doctor(wiki)
        codes = {f["code"] for f in res["findings"]}
        self.assertIn("topic_page_kind_mismatch", codes)

if __name__ == "__main__":
    unittest.main()
