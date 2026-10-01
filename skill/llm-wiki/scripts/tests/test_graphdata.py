import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import graphdata
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import TxnError


def fresh(project="demo"):
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, project)
    return wiki


def put_claim(wiki, cid, subject, predicate, value, status="accepted",
              evidence=None, authority=None, **extra):
    claim = {
        "id": cid,
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "scope": {},
        "valid_from": None,
        "valid_to": None,
        "recorded_at": "2026-10-01T00:00:00Z",
        "status": status,
        "authority": authority or {"type": "explicit_project_decision",
                                   "source": "fixture"},
        "evidence": evidence or [],
        "supersedes": [],
        "version": 1,
    }
    claim.update(extra)
    os.makedirs(wiki.p("claims"), exist_ok=True)
    with open(wiki.p("claims/%s.json" % cid), "w") as f:
        json.dump(claim, f)


def put_source(wiki, sid, origin="docs/a.md", title=None):
    sdir = wiki.p("sources/%s" % sid)
    os.makedirs(sdir, exist_ok=True)
    with open(os.path.join(sdir, "manifest.json"), "w") as f:
        json.dump({"id": sid, "kind": "file", "origin": origin,
                   "created_at": "2026-10-01T00:00:00Z",
                   "versions": [{"version": 1, "sha256": "ab"}],
                   "title": title or os.path.basename(origin)}, f)


def put_decision(wiki, did, title, claim_ids=(), evidence=()):
    os.makedirs(wiki.p("decisions"), exist_ok=True)
    with open(wiki.p("decisions/%s.json" % did), "w") as f:
        json.dump({"id": did, "title": title, "status": "accepted",
                   "decided_on": "2026-10-01",
                   "recorded_at": "2026-10-01T00:00:00Z", "version": 1,
                   "claims": list(claim_ids), "evidence": list(evidence)}, f)


def payload(wiki):
    return graphdata.graph_payload(wiki)


def by_kind(p, kind):
    return [n for n in p["nodes"] if n["kind"] == kind]


def links(p, ltype):
    return [l for l in p["links"] if l["type"] == ltype]


class TestGraphData(unittest.TestCase):
    def test_uninitialized_raises(self):
        wiki = Wiki(tempfile.mkdtemp())
        with self.assertRaises(TxnError):
            payload(wiki)

    def test_empty_wiki_valid_payload(self):
        p = payload(fresh())
        self.assertEqual(p["nodes"], [])
        self.assertEqual(p["links"], [])
        self.assertEqual(p["claim_count"], 0)
        self.assertIn("generated_at", p)

    def test_entity_grouping_and_label_strip(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo.pm97.role", "handles", "transitions")
        put_claim(wiki, "claim_b", "demo.pm97.db", "engine", "orders")
        put_claim(wiki, "claim_c", "demo.orders.engine", "db", "postgres")
        p = payload(wiki)
        ents = by_kind(p, "entity")
        self.assertEqual(len(ents), 2)
        pm = [n for n in ents if n["key"] == "demo.pm97"][0]
        self.assertEqual(pm["label"], "pm97")
        self.assertEqual(pm["claim_count"], 2)
        self.assertEqual(len(pm["claims"]), 2)

    def test_evidence_edge_to_source(self):
        wiki = fresh("demo")
        put_source(wiki, "source_aaa", origin="docs/design.md")
        put_claim(wiki, "claim_a", "demo.pm97.role", "handles", "transitions",
                  evidence=[{"source_id": "source_aaa", "version": 1,
                             "locator": {"type": "heading", "value": "# X"}}])
        p = payload(wiki)
        self.assertEqual(len(by_kind(p, "source")), 1)
        ev = links(p, "evidence")
        self.assertEqual(len(ev), 1)
        src = by_kind(p, "source")[0]
        self.assertEqual(ev[0]["target"], src["id"])
        self.assertEqual(src["title"], "design.md")
        self.assertEqual(src["origin"], "docs/design.md")

    def test_unknown_evidence_source_no_node(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo.pm97.role", "handles", "transitions",
                  evidence=[{"source_id": "source_gone", "version": 1}])
        p = payload(wiki)
        self.assertEqual(by_kind(p, "source"), [])
        self.assertEqual(links(p, "evidence"), [])

    def test_contains_edge(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo", "x", 0)          # key "demo"
        put_claim(wiki, "claim_b", "demo.pm97.role", "x", 1)  # key "demo.pm97"
        p = payload(wiki)
        co = links(p, "contains")
        self.assertEqual(len(co), 1)
        ids = {n["key"]: n["id"] for n in by_kind(p, "entity")}
        self.assertEqual((co[0]["source"], co[0]["target"]),
                         (ids["demo"], ids["demo.pm97"]))


    def test_mentions_edge(self):
        wiki = fresh("demo")
        for i in range(2):
            put_claim(wiki, "claim_a%d" % i, "demo.modalpha.p%d" % i, "x", i)
        for i in range(2):
            put_claim(wiki, "claim_b%d" % i, "demo.consumer.p%d" % i,
                      "uses modalpha", "modalpha modalpha")
        p = payload(wiki)
        me = links(p, "mentions")
        ids = {n["key"]: n["id"] for n in by_kind(p, "entity")}
        self.assertEqual(len(me), 1)
        self.assertEqual(me[0]["source"], ids["demo.modalpha"])
        self.assertEqual(me[0]["target"], ids["demo.consumer"])

    def test_decision_node_and_edge(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo.pm97.role", "handles", "transitions")
        put_decision(wiki, "decision_d1", "Ship pm97", claim_ids=["claim_a"])
        p = payload(wiki)
        dec = by_kind(p, "decision")
        self.assertEqual(len(dec), 1)
        self.assertEqual(dec[0]["label"], "Ship pm97")
        de = links(p, "decision")
        self.assertEqual(len(de), 1)
        self.assertEqual(de[0]["source"], dec[0]["id"])

    def test_wtype_map_override(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo.pm97.role", "x", 1)
        with open(wiki.p("graph-types.json"), "w") as f:
            json.dump({"pm97": "ticket"}, f)
        p = payload(wiki)
        self.assertEqual(by_kind(p, "entity")[0]["wtype"], "ticket")

    def test_wtype_regex_fallback_and_default(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo.pm97.role", "x", 1)
        put_claim(wiki, "claim_b", "demo.orders.engine", "x", 2)
        p = payload(wiki)
        wt = {n["key"]: n["wtype"] for n in by_kind(p, "entity")}
        self.assertEqual(wt["demo.pm97"], "ticket")
        self.assertEqual(wt["demo.orders"], "domain")

    def test_malformed_type_map_warns(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo.pm97.role", "x", 1)
        with open(wiki.p("graph-types.json"), "w") as f:
            f.write("[1,2")
        p = payload(wiki)
        self.assertTrue(p["warnings"])
        self.assertEqual(by_kind(p, "entity")[0]["wtype"], "ticket")

    def test_superseded_excluded_from_edges_but_present(self):
        wiki = fresh("demo")
        put_source(wiki, "source_aaa")
        put_claim(wiki, "claim_old", "demo.pm97.role", "handles", "old",
                  status="superseded",
                  evidence=[{"source_id": "source_aaa", "version": 1}])
        p = payload(wiki)
        ent = by_kind(p, "entity")[0]
        self.assertEqual(ent["claims"][0]["status"], "superseded")
        self.assertEqual(links(p, "evidence"), [])
        src = by_kind(p, "source")
        self.assertEqual(len(src), 1)  # manifest still a node

    def test_label_strips_project_prefix_only(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "alpha.pm97.role", "x", 1)
        put_claim(wiki, "claim_b", "demo.pm97.role", "x", 2)
        # project=demo strips only demo.*; alpha.pm97 stays "alpha.pm97"
        p = payload(wiki)
        labels = {n["key"]: n["label"] for n in by_kind(p, "entity")}
        self.assertEqual(labels["demo.pm97"], "pm97")
        self.assertEqual(labels["alpha.pm97"], "alpha.pm97")

    def test_determinism(self):
        wiki = fresh("demo")
        put_source(wiki, "source_aaa")
        put_claim(wiki, "claim_a", "demo.pm97.role", "x", 1,
                  evidence=[{"source_id": "source_aaa", "version": 1}])
        p1 = payload(wiki)
        p2 = payload(wiki)
        p1.pop("generated_at")
        p2.pop("generated_at")
        self.assertEqual(json.dumps(p1, sort_keys=True),
                         json.dumps(p2, sort_keys=True))

    def test_node_id_uniqueness_and_link_refs(self):
        wiki = fresh("demo")
        put_source(wiki, "source_aaa")
        put_source(wiki, "source_bbb", origin="docs/b.md")
        put_claim(wiki, "claim_a", "demo.pm97.role", "x", 1,
                  evidence=[{"source_id": "source_aaa", "version": 1},
                            {"source_id": "source_bbb", "version": 1}])
        put_decision(wiki, "decision_d1", "D1", claim_ids=["claim_a"],
                     evidence=[{"source_id": "source_aaa", "version": 1}])
        p = payload(wiki)
        ids = [n["id"] for n in p["nodes"]]
        self.assertEqual(sorted(ids), list(range(len(ids))))
        for l in p["links"]:
            self.assertIn(l["source"], ids)
            self.assertIn(l["target"], ids)


if __name__ == "__main__":
    unittest.main()
