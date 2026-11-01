import json, os, tempfile, unittest
from evals.deterministic.check_results_schema import check_rows

SCHEMA = os.path.join(os.path.dirname(__file__), "..", "semantic", "results.schema.json")



class TestResultsSchema(unittest.TestCase):
    def test_valid_row_passes(self):
        row = {"scenario": "correction", "timestamp": "2026-10-31T00:00:00Z",
               "engine_commit": "76e2cb4", "pass": True,
               "answer": "a", "citations": []}
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write(json.dumps(row) + "\n")
            path = f.name
        ok, errors = check_rows(SCHEMA, path)
        os.unlink(path)
        self.assertTrue(ok, errors)

    def test_missing_engine_commit_fails(self):
        row = {"scenario": "correction", "timestamp": "2026-10-31T00:00:00Z",
               "pass": True, "answer": "a", "citations": []}
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write(json.dumps(row) + "\n")
            path = f.name
        ok, errors = check_rows(SCHEMA, path)
        os.unlink(path)
        self.assertFalse(ok)
        self.assertTrue(any("engine_commit" in e for e in errors))

    def test_bad_scenario_fails(self):
        row = {"scenario": "not_a_scenario", "timestamp": "t",
               "engine_commit": "abc1234", "pass": True,
               "answer": "a", "citations": []}
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write(json.dumps(row) + "\n")
            path = f.name
        ok, errors = check_rows(SCHEMA, path)
        os.unlink(path)
        self.assertFalse(ok)

    def test_non_object_line_fails(self):
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write("42\n")
            path = f.name
        ok, errors = check_rows(SCHEMA, path)
        os.unlink(path)
        self.assertFalse(ok)
        self.assertTrue(any("not a JSON object" in e for e in errors))


if __name__ == "__main__":
    unittest.main()
