import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import deps, graphdata, review, synthesize
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError

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

def page_md(dep_list, body="We run postgres.", type_="concept"):
    lines = ["---", "type: %s" % type_, "subject: db.engine", "title: DB engine",
             "created: 2026-09-15T00:00:00Z", "updated: 2026-09-15T00:00:00Z"]
    if dep_list:
        lines.append("deps:")
        for d in dep_list:
            lines.append("  - %s" % d)
    else:
        lines.append("deps: []")
    lines += ["stale: false", "---", "", body, ""]
    return "\n".join(lines)

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
        self.assertEqual(p["meta"]["review"], {"open": 0, "deferred": 0, "total": 0})
        self.assertEqual(p["meta"]["generated_at"], p["generated_at"])

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

    def test_mentions_inflected_and_short_alias(self):
        # Q8: 3-char alias never matches; 4-char alias matches inflected forms;
        # stop filtering is stem-based ("orders" filtered like "order")
        wiki = fresh("demo")
        for i in range(2):
            put_claim(wiki, "claim_s%d" % i, "demo.abc.p%d" % i, "x", i)
        for i in range(2):
            put_claim(wiki, "claim_t%d" % i, "demo.user.p%d" % i,
                      "uses abc", "abc abc")
        for i in range(2):
            put_claim(wiki, "claim_u%d" % i, "demo.orders.p%d" % i, "x", i)
        for i in range(2):
            put_claim(wiki, "claim_v%d" % i, "demo.fulfill.p%d" % i,
                      "tracks orders", "orders orders")
        p = payload(wiki)
        self.assertEqual(links(p, "mentions"), [])
        # override the stop list: "orders" becomes matchable
        old = os.environ.get("MENTION_STOP")
        try:
            os.environ["MENTION_STOP"] = ""
            p2 = payload(wiki)
            me2 = links(p2, "mentions")
            ids = {n["key"]: n["id"] for n in by_kind(p2, "entity")}
            self.assertEqual(len(me2), 1)
            self.assertEqual((me2[0]["source"], me2[0]["target"]),
                             (ids["demo.orders"], ids["demo.fulfill"]))
        finally:
            if old is None:
                os.environ.pop("MENTION_STOP", None)
            else:
                os.environ["MENTION_STOP"] = old

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
        p = payload(wiki)
        ids = [n["id"] for n in p["nodes"]]
        self.assertEqual(sorted(ids), list(range(len(ids))))
        for l in p["links"]:
            self.assertIn(l["source"], ids)
            self.assertIn(l["target"], ids)

class TestReviewSurface(unittest.TestCase):
    def test_node_flags_review_disputed_stale_orphan(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_a", "demo.pm97.role", "x", 1,               # reviewed
                  evidence=[{"source_id": "source_aaa", "version": 1}])
        put_claim(wiki, "claim_b", "demo.pm97.db", "engine", "old",
                  status="superseded", superseded_by="claim_c")
        put_claim(wiki, "claim_c", "demo.pm97.db", "engine", "new")
        put_claim(wiki, "claim_d", "demo.billing.sum", "x", 1, status="disputed")
        put_source(wiki, "source_aaa")  # linked via claim_a evidence
        txn = Transaction(wiki, "t")
        review.add_item(txn, "contradiction", "role may change", [],
                        ["claim_a@1"], "low")
        txn.commit(wiki.revision())
        p = payload(wiki)
        flags = {n["key"]: n["flags"] for n in by_kind(p, "entity")}
        self.assertTrue(flags["demo.pm97"]["review_open"])
        self.assertTrue(flags["demo.billing"]["disputed"])
        # pm97 has accepted + superseded claims: mixed -> not stale
        self.assertFalse(flags["demo.pm97"]["stale"])
        src_flags = by_kind(p, "source")[0]["flags"]
        self.assertFalse(src_flags["orphan"])
        self.assertFalse(flags["demo.pm97"]["orphan"])
        self.assertTrue(flags["demo.billing"]["orphan"])
        self.assertEqual(p["meta"]["review"]["open"], 1)
        self.assertEqual(p["meta"]["review"]["total"], 1)
        self.assertTrue(all("flags" in n for n in p["nodes"]))

    def test_fully_terminal_entity_is_stale(self):
        wiki = fresh("demo")
        put_claim(wiki, "claim_old", "demo.legacy.x", "x", 1, status="rejected")
        p = payload(wiki)
        flags = by_kind(p, "entity")[0]["flags"]
        self.assertTrue(flags["stale"])
        self.assertFalse(flags["superseded"])

class TestPageNodes(unittest.TestCase):
    def test_concept_page_documents_claim_entity(self):
        wiki = fresh()
        put_claim(wiki, "claim_a", "demo.db.engine", "value", "postgres")
        dep = "claim_a@1"
        os.makedirs(wiki.p("wiki/concepts"), exist_ok=True)
        with open(wiki.p("wiki/concepts/index.md"), "w") as f:
            f.write("# Concepts\n")
        synthesize.write_page(wiki, "wiki/concepts/db-engine.md",
                              page_md([dep], body="We run [[claim_a]]."),
                              [dep], wiki.revision())
        p = payload(wiki)
        pgs = by_kind(p, "page")
        self.assertEqual(len(pgs), 1)  # index.md excluded
        pg = pgs[0]
        self.assertEqual(pg["kind"], "page")
        self.assertEqual(pg["wtype"], "page_concept")
        self.assertEqual(pg["key"], "wiki/concepts/db-engine.md")
        self.assertEqual(pg["path"], "wiki/concepts/db-engine.md")
        self.assertEqual(pg["label"], "DB engine")
        self.assertEqual(p["page_count"], 1)
        docs = links(p, "documents")
        self.assertEqual(len(docs), 1)
        self.assertEqual(docs[0]["source"], by_kind(p, "entity")[0]["id"])
        self.assertEqual(docs[0]["target"], pg["id"])

    def test_stale_flag_and_slug_label(self):
        wiki = fresh()
        os.makedirs(wiki.p("wiki/questions"), exist_ok=True)
        with open(wiki.p("wiki/questions/how-to-auth.md"), "w") as f:
            f.write("---\ntype: question\ndeps: []\nstale: true\n---\n"
                    "\nHow?\n")
        os.makedirs(wiki.p("wiki/sources"), exist_ok=True)
        with open(wiki.p("wiki/sources/notes.md"), "w") as f:
            f.write("---\ntype: source\ndeps: []\nstale: false\n---\n"
                    "\nNotes.\n")
        p = payload(wiki)
        self.assertEqual(p["page_count"], 2)
        by_wtype = {n["wtype"]: n for n in by_kind(p, "page")}
        q = by_wtype["page_question"]
        self.assertEqual(q["label"], "how-to-auth")
        self.assertIn({"p": "type", "v": "question"}, q["summary"])
        self.assertIn({"p": "stale", "v": True}, q["summary"])
        self.assertIn({"p": "stale", "v": False},
                      by_wtype["page_source"]["summary"])

    def test_decision_dep_documents_edge(self):
        wiki = fresh()
        put_decision(wiki, "decision_d1", "Ship it")
        os.makedirs(wiki.p("wiki/changes"), exist_ok=True)
        with open(wiki.p("wiki/changes/db-switch.md"), "w") as f:
            f.write("---\ntype: change\ntitle: DB switch\n"
                    "deps:\n  - decision_d1@1\nstale: false\n---\n"
                    "\nSwitched.\n")
        txn = Transaction(wiki, "deps")
        deps.register(txn, "wiki/changes/db-switch.md", ["decision_d1@1"])
        txn.commit(wiki.revision())
        p = payload(wiki)
        docs = links(p, "documents")
        self.assertEqual(len(docs), 1)
        self.assertEqual(docs[0]["source"], by_kind(p, "decision")[0]["id"])
        self.assertEqual(docs[0]["target"], by_kind(p, "page")[0]["id"])

    def test_depends_on_and_unresolved_deps_skipped(self):
        wiki = fresh()
        put_claim(wiki, "claim_a", "demo.db.engine", "value", "postgres")
        dep = "claim_a@1"
        os.makedirs(wiki.p("wiki/procedures"), exist_ok=True)
        with open(wiki.p("wiki/procedures/failover.md"), "w") as f:
            f.write("---\ntype: procedure\ndeps: []\nstale: false\n---\n"
                    "\nSteps.\n")
        synthesize.write_page(wiki, "wiki/concepts/db-engine.md",
                              page_md([dep], body="We run [[claim_a]]."),
                              [dep], wiki.revision())
        txn = Transaction(wiki, "deps")
        deps.register(txn, "wiki/procedures/failover.md",
                      ["wiki/concepts/db-engine.md", "claim_gone@1"])
        txn.commit(wiki.revision())
        p = payload(wiki)
        ids = {n["key"]: n["id"] for n in by_kind(p, "page")}
        self.assertEqual(len(ids), 2)
        dep_links = links(p, "depends_on")
        self.assertEqual(len(dep_links), 1)
        self.assertEqual(dep_links[0]["source"],
                         ids["wiki/concepts/db-engine.md"])
        self.assertEqual(dep_links[0]["target"],
                         ids["wiki/procedures/failover.md"])
        # unresolvable claim_gone@1 silently skipped, claim_a edge intact
        self.assertEqual(len(links(p, "documents")), 1)

if __name__ == "__main__":
    unittest.main()
