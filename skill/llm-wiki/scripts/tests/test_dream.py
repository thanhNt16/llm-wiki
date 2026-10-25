import datetime
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, dream, review
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction

SID = "source_" + "A" * 26
NOW = datetime.date(2026, 10, 25)


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "dream-test")
    return wiki


def claim(wiki, subject="s", predicate="p", value=1, status="candidate",
          evidence_sources=((SID, 1),), **kw):
    evidence = [{"source_id": sid, "source_version": ver,
                 "locator": {"type": "text", "value": "l"}}
                for sid, ver in evidence_sources]
    c = {
        "id": "claim_" + claims.ids.new("claim").split("_", 1)[1],
        "subject": subject, "predicate": predicate, "value": value,
        "scope": kw.get("scope", {}),
        "valid_from": kw.get("valid_from"),
        "valid_to": kw.get("valid_to"),
        "recorded_at": kw.get("recorded_at", "2026-09-15T00:00:00Z"),
        "status": status,
        "authority": {"type": "agent_inference"},
        "evidence": evidence,
        "supersedes": kw.get("supersedes", []),
        "version": 1,
    }
    if status in claims.TERMINAL:
        c["superseded_by"] = kw.get("superseded_by", "claim_" + "Z" * 26)
    return c
def save(wiki, *cs):
    txn = Transaction(wiki, "test")
    for c in cs:
        claims.save_claim(txn, c)
    txn.commit(wiki.revision())


def seed_source(wiki, sid):
    d = wiki.p("sources", sid)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "manifest.json"), "w") as f:
        json.dump({"id": sid, "kind": "file", "status": "extracted"}, f)
    return d


class TestDream(unittest.TestCase):
    def test_dry_run_changes_nothing(self):
        wiki = fresh()
        c = claim(wiki, status="superseded", superseded_by="claim_x")
        save(wiki, c)
        rev = wiki.revision()
        out = dream.run(wiki, apply=False, now=NOW)
        self.assertFalse(out["applied"])
        self.assertEqual(len(out["phases"]["supersede"]["archive"]), 1)
        self.assertEqual(wiki.revision(), rev)
        self.assertEqual(len(claims.list_claims(wiki)), 1)

    def test_superseded_claim_archived(self):
        wiki = fresh()
        old = claim(wiki, status="superseded")
        old["superseded_by"] = "claim_zzz"
        new = claim(wiki, subject="s")
        save(wiki, old, new)
        out = dream.run(wiki, apply=True, now=NOW)
        self.assertIn(old["id"], out["archived"])
        live = {c["id"] for c in claims.list_claims(wiki)}
        self.assertNotIn(old["id"], live)
        self.assertIn(new["id"], live)
        arc = wiki.load_json("claims/_archive/%s.json" % old["id"])
        self.assertEqual(arc["dream_reason"], "superseded_by claim_zzz")
        self.assertTrue(arc["dreamed_at"].startswith("2026-10-25"))

    def test_expired_claim_archived_future_kept(self):
        wiki = fresh()
        dead = claim(wiki, status="accepted", valid_to="2026-01-01")
        alive = claim(wiki, subject="s2", status="accepted",
                      valid_to="2027-01-01")
        save(wiki, dead, alive)
        out = dream.run(wiki, apply=True, now=NOW)
        self.assertIn(dead["id"], out["archived"])
        live = {c["id"] for c in claims.list_claims(wiki)}
        self.assertIn(alive["id"], live)

    def test_duplicates_keep_newest(self):
        wiki = fresh()
        old = claim(wiki, subject="s", status="accepted",
                    recorded_at="2026-01-01T00:00:00Z")
        new = claim(wiki, subject="s", status="accepted",
                    recorded_at="2026-09-01T00:00:00Z")
        save(wiki, old, new)
        out = dream.run(wiki, apply=True, now=NOW)
        self.assertIn(old["id"], out["archived"])
        live = {c["id"] for c in claims.list_claims(wiki)}
        self.assertIn(new["id"], live)

    def test_evidenceless_candidate_archived_review_open_skipped(self):
        wiki = fresh()
        naked = claim(wiki, evidence_sources=())
        naked["evidence"] = []
        reviewed = claim(wiki, subject="s2", evidence_sources=())
        reviewed["evidence"] = []
        save(wiki, naked, reviewed)
        qpath = wiki.p(review.QUEUE)
        os.makedirs(os.path.dirname(qpath), exist_ok=True)
        with open(qpath, "w") as f:
            f.write(json.dumps({"id": "review_x", "status": "open",
                                "affected": [reviewed["id"]],
                                "kind": "possible_contradiction"}) + "\n")
        out = dream.run(wiki, apply=True, now=NOW)
        self.assertIn(naked["id"], out["archived"])
        skipped = {s["id"] for s in out["skipped"]["orphan"]}
        self.assertIn(reviewed["id"], skipped)

    def test_uncited_source_moved_cited_kept(self):
        wiki = fresh()
        dead_sid = "source_" + "D" * 26
        seed_source(wiki, SID)
        seed_source(wiki, dead_sid)
        c = claim(wiki)  # cites SID
        save(wiki, c)
        out = dream.run(wiki, apply=True, now=NOW)
        self.assertFalse(os.path.isdir(wiki.p("sources", dead_sid)))
        self.assertTrue(os.path.isdir(
            wiki.p(".state/dream-archive/sources", dead_sid)))
        self.assertTrue(os.path.isdir(wiki.p("sources", SID)))
        self.assertEqual(out["moved_sources"],
                         [wiki.p(".state/dream-archive/sources", dead_sid)])

    def test_junk_deleted_held_lock_kept(self):
        wiki = fresh()
        staging = wiki.p(".state", "staging", "run_x")
        os.makedirs(staging)
        locks = wiki.p(".state", "locks")
        os.makedirs(locks, exist_ok=True)
        open(os.path.join(locks, "orphan.lock"), "w").close()
        open(wiki.p(".state", "journal.json"), "w").close()
        out = dream.run(wiki, apply=True, now=NOW)
        self.assertFalse(os.path.isdir(staging))
        self.assertFalse(os.path.isfile(wiki.p(".state", "journal.json")))
        self.assertFalse(os.path.isfile(os.path.join(locks, "orphan.lock")))
        self.assertEqual(len(out["deleted"]), 3)

    def test_derived_edge_dropped(self):
        wiki = fresh()
        c = claim(wiki)
        save(wiki, c)
        txn = Transaction(wiki, "test")
        deps_rel = ".state/dependencies.json"
        txn.stage_state(deps_rel, lambda d: d.setdefault(
            "edges", {}).__setitem__("wiki/concepts/s.md", ["claim_ghost"]))
        txn.commit(wiki.revision())
        os.makedirs(os.path.dirname(wiki.p("wiki/concepts/s.md")),
                    exist_ok=True)
        with open(wiki.p("wiki/concepts/s.md"), "w") as f:
            f.write("x")
        out = dream.run(wiki, apply=True, now=NOW)
        self.assertEqual(out["edge_drops"], 1)
        edges = wiki.load_json(deps_rel)["edges"]
        self.assertEqual(edges["wiki/concepts/s.md"], [])

    def test_phase_filter(self):
        wiki = fresh()
        staging = wiki.p(".state", "staging", "run_x")
        os.makedirs(staging)
        old = claim(wiki, status="superseded")
        save(wiki, old)
        out = dream.run(wiki, apply=True, phases=("junk",), now=NOW)
        self.assertFalse(os.path.isdir(staging))
        self.assertEqual(out["archived"], [])  # supersede skipped
        self.assertEqual(len(claims.list_claims(wiki)), 1)

    def test_purge_90d_deletes_referenced_skips(self):
        wiki = fresh()
        old = claim(wiki, status="superseded")
        old["superseded_by"] = "claim_live"
        save(wiki, old)
        dream.run(wiki, apply=True, now=NOW)  # archives old @ 2026-10-25

        # still_referenced: a live claim supersedes nothing but holds the id
        c2 = claim(wiki, subject="keep")
        c2["supersedes"] = [old["id"]]
        save(wiki, c2)

        future = NOW + datetime.timedelta(days=91)
        out = dream.purge(wiki, dry_run=True, now=future)
        self.assertEqual(out["purge"]["purged"], [])
        self.assertEqual(out["purge"]["skipped"][0]["reason"],
                         "still_referenced")

        # drop the reference, purge again
        c2["supersedes"] = []
        txn = Transaction(wiki, "test")
        claims.save_claim(txn, c2)
        txn.commit(wiki.revision())
        out = dream.purge(wiki, dry_run=False, now=future)
        self.assertTrue(out["purge"]["purged"])
        self.assertFalse(os.path.isfile(
            wiki.p("claims/_archive/%s.json" % old["id"])))

    def test_purge_too_recent_skipped(self):
        wiki = fresh()
        old = claim(wiki, status="superseded")
        save(wiki, old)
        dream.run(wiki, apply=True, now=NOW)
        out = dream.purge(wiki, dry_run=True,
                          now=NOW + datetime.timedelta(days=30))
        self.assertEqual(out["purge"]["skipped"][0]["reason"], "too_recent")


if __name__ == "__main__":
    unittest.main()
