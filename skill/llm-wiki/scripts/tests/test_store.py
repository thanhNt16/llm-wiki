import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore.store import Wiki, init_wiki

REQUIRED_DIRS = [
    "raw/files", "raw/urls", "raw/repositories", "raw/sessions", "raw/text",
    "sources", "claims", "decisions", "notes",
    "wiki/concepts", "wiki/entities", "wiki/decisions", "wiki/procedures",
    "wiki/questions", "wiki/changes",
    "context", "views",
    ".state/locks",
]


class TestStore(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp()
        self.wiki = Wiki(self.root)

    def test_init_creates_layout(self):
        status = init_wiki(self.wiki, "demo")
        self.assertTrue(status["initialized"])
        for d in REQUIRED_DIRS:
            self.assertTrue(os.path.isdir(self.wiki.p(d)), d)
        self.assertTrue(os.path.isfile(self.wiki.p("wiki.json")))
        self.assertTrue(os.path.isfile(self.wiki.p(".state/manifest.json")))
        self.assertTrue(os.path.isfile(self.wiki.p(".state/aliases.json")))
        self.assertTrue(os.path.isfile(self.wiki.p(".state/dependencies.json")))
        self.assertTrue(os.path.isfile(self.wiki.p(".state/graph.json")))
        self.assertTrue(os.path.isfile(self.wiki.p(".state/operations.jsonl")))
        self.assertTrue(os.path.isfile(self.wiki.p(".state/review-queue.jsonl")))

    def test_init_idempotent_non_destructive(self):
        init_wiki(self.wiki, "demo")
        with open(self.wiki.p("wiki.json"), "a") as f:
            f.write("\n")
        status = init_wiki(self.wiki, "demo")
        self.assertFalse(status["initialized"])

    def test_config_defaults(self):
        init_wiki(self.wiki, "demo")
        cfg = self.wiki.load_config()
        self.assertEqual(cfg["project"], "demo")
        self.assertFalse(cfg["allow_cross_project"])
        self.assertEqual(cfg["secret_policy"], "warn")
        self.assertIn("explicit_project_decision", cfg["acceptance_policy"]["auto_accept_authority"])
        self.assertEqual(cfg["context_budget_default"], 6000)

    def test_revision_counter(self):
        init_wiki(self.wiki, "demo")
        self.assertEqual(self.wiki.revision(), 0)
        s = self.wiki.state()
        s["revision"] = 7
        self.wiki.save_state(s)
        self.assertEqual(self.wiki.revision(), 7)

    def test_exists_false_before_init(self):
        self.assertFalse(self.wiki.exists())


class InitPageKindsTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp()
        self.wiki = Wiki(self.root)

    def test_init_writes_starter_file(self):
        init_wiki(self.wiki, "demo")
        path = self.wiki.p("page-kinds.json")
        self.assertTrue(os.path.isfile(path))
        with open(path) as f:
            data = json.load(f)
        self.assertEqual(data["rules"], [])
        self.assertEqual(data["default"], "concepts")
        self.assertIn("_comment", data)

    def test_init_does_not_clobber(self):
        init_wiki(self.wiki, "demo")
        path = self.wiki.p("page-kinds.json")
        with open(path, "w") as f:
            f.write('{"rules": [{"kind": "entities", "prefix": "svc."}]}')
        # init_wiki early-returns while wiki.json exists; remove the sentinel
        # so a second init runs the scaffold path again.
        os.remove(self.wiki.p("wiki.json"))
        init_wiki(self.wiki, "demo")
        with open(path) as f:
            self.assertEqual(f.read(),
                             '{"rules": [{"kind": "entities", "prefix": "svc."}]}')


if __name__ == "__main__":
    unittest.main()
