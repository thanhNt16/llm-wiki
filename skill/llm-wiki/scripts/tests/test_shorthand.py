import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore.shorthand import expand_locator, expand_authority, \
    normalize_candidate, parse_candidates_file
from wikicore.transaction import TxnError


class TestShorthand(unittest.TestCase):
    def test_heading_locator(self):
        self.assertEqual(expand_locator("h:1.1 Intro"),
                         {"type": "heading", "value": "1.1 Intro"})

    def test_line_range_locator(self):
        self.assertEqual(expand_locator("l:139"), {"type": "line_range", "value": "139"})
        self.assertEqual(expand_locator("l:557-559"),
                         {"type": "line_range", "value": "557-559"})

    def test_section_locator_and_passthrough(self):
        self.assertEqual(expand_locator("s:Body"), {"type": "section", "value": "Body"})
        d = {"type": "paragraph", "value": "3"}
        self.assertEqual(expand_locator(d), d)

    def test_bad_locator_rejected(self):
        with self.assertRaises(TxnError):
            expand_locator("x:bogus")

    def test_authority_shorthand(self):
        self.assertEqual(expand_authority("manual"),
                         {"type": "authoritative_source", "source": "manual"})
        self.assertEqual(expand_authority("decision"),
                         {"type": "explicit_project_decision", "source": "decision"})
        self.assertEqual(expand_authority("code"),
                         {"type": "implementation_verification", "source": "code"})
        self.assertEqual(expand_authority("inferred"),
                         {"type": "agent_inference", "source": "agent"})
        self.assertEqual(expand_authority("adr:ADR-019"),
                         {"type": "explicit_project_decision", "source": "ADR-019"})

    def test_authority_passthrough(self):
        d = {"type": "authoritative_source", "source": "Manual v1.2"}
        self.assertEqual(expand_authority(d), d)

    def test_bad_authority_rejected(self):
        with self.assertRaises(TxnError):
            expand_authority("bogus")

    def test_normalize_stamps_engine_fields(self):
        c = normalize_candidate(
            {"subject": "a.b", "predicate": "p", "value": 7,
             "locator": "h:Window", "authority": "decision"},
            "source_X", 2, "origin_X")
        self.assertEqual(c["status"], "candidate")
        self.assertEqual(c["proposed_by"], "agent")
        self.assertEqual(c["root_origin"], "origin_X")
        self.assertEqual(c["evidence"], [{
            "source_id": "source_X", "source_version": 2,
            "locator": {"type": "heading", "value": "Window"}}])
        self.assertIn("recorded_at", c)
        self.assertNotIn("id", c)  # staging assigns ids, not shorthand

    def test_normalize_requires_subject_predicate(self):
        with self.assertRaises(TxnError):
            normalize_candidate({"predicate": "p"}, "s", 1, "o")

    def test_parse_jsonl(self):
        p = os.path.join(tempfile.mkdtemp(), "c.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a","predicate":"b","value":1,"locator":"h:H"}\n')
            f.write('{"subject":"c","predicate":"d","value":2,"locator":"l:5"}\n')
        self.assertEqual(len(parse_candidates_file(p)), 2)

    def test_parse_jsonl_bad_line_names_line(self):
        p = os.path.join(tempfile.mkdtemp(), "c.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a","predicate":"b"}\nnot json\n')
        with self.assertRaises(TxnError) as ctx:
            parse_candidates_file(p)
        self.assertIn("line 2", str(ctx.exception))

    def test_parse_verbose_json_still_works(self):
        p = os.path.join(tempfile.mkdtemp(), "c.json")
        with open(p, "w") as f:
            json.dump([{"subject": "a", "predicate": "b", "value": 1}], f)
        self.assertEqual(len(parse_candidates_file(p)), 1)
