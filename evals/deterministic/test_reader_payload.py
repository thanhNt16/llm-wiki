"""SP3-T9: viz prose reader — deterministic gates.

graph-data: page nodes carry ``body`` (markdown text after frontmatter,
capped at 20000 chars) so the exported payload is self-contained prose.
dist/index.html: vanilla reader pane (id="reader") that renders the
selected page node's label + body; non-page nodes keep current behavior.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
WIKI = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")
DIST = os.path.join(REPO, "skill", "wiki-visualize", "assets",
                    "graph-ui", "dist", "index.html")


def _run(root, *args):
    return subprocess.run([sys.executable, WIKI, "--root", root, *args],
                          capture_output=True, text=True)


def _init_wiki(root):
    r = _run(root, "init", "--name", "gate")
    assert r.returncode == 0, r.stderr


def _author_page(root, name, text):
    pdir = os.path.join(root, ".llm-wiki", "wiki", "concepts")
    os.makedirs(pdir, exist_ok=True)
    with open(os.path.join(pdir, name), "w") as f:
        f.write(text)


def _graph_data(root):
    r = _run(root, "graph-data")
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def _page_node(payload, rel):
    for n in payload["nodes"]:
        if n.get("kind") == "page" and n.get("key") == rel:
            return n
    return None


class TestReaderPayload(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="reader-gate-")
        self.addCleanup(lambda: __import__("shutil").rmtree(self.root, True))
        _init_wiki(self.root)

    def test_graph_data_carries_page_body(self):
        prose = ("Foo is a deterministic reader-payload test concept.\n\n"
                 "It exists to prove page bodies reach graph-data.json.\n")
        _author_page(self.root, "foo.md",
                     "---\ntype: concept\ntitle: Foo Concept\n"
                     "subject: gate.foo\n---\n" + prose)
        node = _page_node(_graph_data(self.root), "wiki/concepts/foo.md")
        self.assertIsNotNone(node, "page node for wiki/concepts/foo.md missing")
        self.assertIn("body", node, "page node has no 'body' field")
        self.assertTrue(node["body"], "page node body is empty")
        self.assertIn("Foo is a deterministic reader-payload test concept.",
                      node["body"])
        # frontmatter is stripped — body is prose only
        self.assertNotIn("type: concept", node["body"])

    def test_page_body_capped_at_20000(self):
        _author_page(self.root, "big.md",
                     "---\ntype: concept\ntitle: Big\n---\n" + "x" * 25000)
        node = _page_node(_graph_data(self.root), "wiki/concepts/big.md")
        self.assertIsNotNone(node)
        self.assertEqual(len(node["body"]), 20000)
        self.assertEqual(node["body"], "x" * 20000)

    def test_dist_has_reader_pane(self):
        with open(DIST) as f:
            html = f.read()
        self.assertIn('id="reader"', html)
        # page nodes only: kind check + body reference must both appear
        self.assertIn('kind === "page"', html)
        self.assertIn(".body", html)

    def test_dist_reader_selector_scoped_to_detail_panel(self):
        # SP3-F4/T9-B2: bare p.font-mono also matched the 3D hover tooltip's
        # qualified_name (font-mono truncate) — reader popped on hover.
        with open(DIST) as f:
            html = f.read()
        self.assertIn("p.font-mono.break-all.leading-relaxed", html)
        self.assertNotIn('querySelectorAll("p.font-mono")', html)

    def test_dist_reader_has_dedupe_and_latch(self):
        # SP3-F4/T9-B1: observer self-feed froze the page and undid ×/Escape.
        with open(DIST) as f:
            html = f.read()
        self.assertIn("shownKey", html)
        self.assertIn("dismissed", html)


if __name__ == "__main__":
    unittest.main()
