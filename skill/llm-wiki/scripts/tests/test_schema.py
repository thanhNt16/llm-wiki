import json, os, sys, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import schema

SCHEMAS_DIR = os.path.join(
    os.path.dirname(__file__), "..", "..", "schemas"
)


def load_schema(name):
    with open(os.path.join(SCHEMAS_DIR, name)) as f:
        return json.load(f)


GOOD_CLAIM = {
    "id": "claim_" + "A" * 26,
    "subject": "project.analytics.attribution",
    "predicate": "click_lookback_window",
    "value": 7,
    "unit": "days",
    "scope": {"environment": "production"},
    "valid_from": "2026-09-01",
    "valid_to": None,
    "recorded_at": "2026-09-14T10:00:00Z",
    "status": "accepted",
    "authority": {"type": "explicit_project_decision", "source": "ADR-019"},
    "evidence": [
        {
            "source_id": "source_" + "A" * 26,
            "source_version": 3,
            "locator": {"type": "heading", "value": "Attribution Window"},
        }
    ],
    "supersedes": [],
    "version": 1,
}


class TestValidator(unittest.TestCase):
    def setUp(self):
        self.s = {
            "type": "object",
            "required": ["name", "tags"],
            "properties": {
                "name": {"type": "string", "pattern": "^[a-z]+$"},
                "count": {"type": "integer", "minimum": 1, "maximum": 10},
                "tags": {"type": "array", "items": {"type": "string"}, "uniqueItems": True},
                "state": {"enum": ["on", "off", None]},
            },
        }

    def test_valid(self):
        self.assertEqual(schema.validate({"name": "abc", "tags": ["x"]}, self.s), [])

    def test_missing_required(self):
        errors = schema.validate({"tags": []}, self.s)
        self.assertTrue(any("name" in e for e in errors))

    def test_wrong_type(self):
        errors = schema.validate({"name": 5, "tags": []}, self.s)
        self.assertTrue(any("name" in e for e in errors))

    def test_pattern(self):
        errors = schema.validate({"name": "ABC", "tags": []}, self.s)
        self.assertTrue(any("pattern" in e for e in errors))

    def test_enum_with_null(self):
        self.assertEqual(schema.validate({"name": "a", "tags": [], "state": None}, self.s), [])
        errors = schema.validate({"name": "a", "tags": [], "state": "maybe"}, self.s)
        self.assertTrue(any("state" in e for e in errors))

    def test_unique_items(self):
        errors = schema.validate({"name": "a", "tags": ["x", "x"]}, self.s)
        self.assertTrue(any("unique" in e for e in errors))

    def test_minimum(self):
        errors = schema.validate({"name": "a", "tags": [], "count": 0}, self.s)
        self.assertTrue(any("count" in e for e in errors))


class TestCanonicalSchemas(unittest.TestCase):
    def test_good_claim_passes(self):
        self.assertEqual(schema.validate(GOOD_CLAIM, load_schema("claim.schema.json")), [])

    def test_claim_bad_status(self):
        bad = dict(GOOD_CLAIM, status="true-ish")
        self.assertTrue(schema.validate(bad, load_schema("claim.schema.json")))

    def test_claim_bad_id(self):
        bad = dict(GOOD_CLAIM, id="claim_xyz")
        self.assertTrue(schema.validate(bad, load_schema("claim.schema.json")))

    def test_all_eight_exist_and_load(self):
        expected = [
            "claim.schema.json",
            "decision.schema.json",
            "source-manifest.schema.json",
            "extraction-report.schema.json",
            "candidate-claim.schema.json",
            "context-receipt.schema.json",
            "run-receipt.schema.json",
            "review-item.schema.json",
        ]
        for name in expected:
            s = load_schema(name)
            self.assertIn("type", s, name)
            self.assertIn("required", s, name)

    def test_candidate_claim_requires_candidate_status(self):
        s = load_schema("candidate-claim.schema.json")
        good = dict(GOOD_CLAIM, status="candidate", proposed_by="agent")
        good.pop("version", None)
        good.pop("acceptance", None)
        self.assertEqual(schema.validate(good, s), [])
        self.assertTrue(schema.validate(dict(good, status="accepted"), s))


if __name__ == "__main__":
    unittest.main()
