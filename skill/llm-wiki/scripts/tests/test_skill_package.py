import os, re, sys, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

SKILL_DIR = os.path.join(os.path.dirname(__file__), "..", "..")

import wiki as cli  # noqa: E402


class TestSkillPackage(unittest.TestCase):
    def setUp(self):
        with open(os.path.join(SKILL_DIR, "SKILL.md")) as f:
            self.skill = f.read()

    def test_frontmatter_name_and_description(self):
        m = re.match(r"^---\nname:\s*(\S+)\ndescription:\s*(.+?)\n---\n", self.skill, re.S)
        self.assertIsNotNone(m, "frontmatter must have name and description")
        self.assertEqual(m.group(1), "llm-wiki")
        self.assertLessEqual(len(m.group(1) + m.group(2)), 1024)

    def test_description_starts_with_use_when(self):
        m = re.search(r"^description:\s*(.+)$", self.skill, re.M)
        self.assertTrue(m.group(1).strip().startswith("Use when"),
                        "description must describe triggers, not workflow")

    def test_description_does_not_summarize_workflow(self):
        m = re.search(r"^description:\s*(.+)$", self.skill, re.M)
        desc = m.group(1).lower()
        for phrase in ("first", "then", "step"):
            self.assertNotIn(" %s " % phrase, desc,
                             "description must not narrate a workflow")

    def test_skill_body_is_thin(self):
        body = self.skill.split("---\n", 2)[2]
        self.assertLess(len(body.split()), 600, "SKILL.md should stay under ~600 words")

    def test_all_references_exist(self):
        for ref in ("ARCHITECTURE", "COMMANDS", "RECONCILIATION", "EXTRACTION",
                    "CONTEXT-PACKS", "SECURITY", "EVALUATION"):
            path = os.path.join(SKILL_DIR, "references", "%s.md" % ref)
            self.assertTrue(os.path.isfile(path), ref)
            self.assertGreater(os.path.getsize(path), 400, ref)

    def test_commands_reference_uses_real_subcommands(self):
        with open(os.path.join(SKILL_DIR, "references", "COMMANDS.md")) as f:
            text = f.read()
        parser = cli.build_parser()
        real = set()
        for action in parser._subparsers._group_actions:
            real.update(action.choices.keys())
        invocations = set(re.findall(r"wiki\.py ([a-z-]+)", text))
        unknown = invocations - real
        self.assertEqual(unknown, set(), "COMMANDS.md references unknown subcommands")

    def test_schemas_present(self):
        sdir = os.path.join(SKILL_DIR, "schemas")
        count = len([n for n in os.listdir(sdir) if n.endswith(".schema.json")])
        self.assertGreaterEqual(count, 8)


if __name__ == "__main__":
    unittest.main()
