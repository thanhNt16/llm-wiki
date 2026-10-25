import os, unittest

CORPUS = os.path.join(os.path.dirname(__file__), "..", "golden-corpus")


def read(*parts):
    with open(os.path.join(CORPUS, *parts)) as f:
        return f.read()


class TestCorpus(unittest.TestCase):
    def test_required_files_exist(self):
        required = [
            "README.md",
            "docs/adr-001-postgres.md",
            "docs/adr-007-retries.md",
            "docs/adr-019-attribution.md",
            "docs/spec-attribution-window.pdf",
            "notes/memo-2026-09-01-window-change.md",
            "notes/meeting-2026-09-10.md",
            "notes/failed-redis-pubsub.md",
            "transcripts/session-2026-09-12.md",
            "config/analytics.prod.json",
            "config/analytics.sandbox.json",
            "services/analytics/attribution.py",
            "data/config-matrix.csv",
            "questions.txt",
        ]
        for rel in required:
            self.assertTrue(os.path.isfile(os.path.join(CORPUS, rel)), rel)

    def test_contains_explicit_correction(self):
        memo = read("notes", "memo-2026-09-01-window-change.md")
        self.assertIn("30 days", memo)
        self.assertIn("7 days", memo)
        self.assertIn("2026-09-01", memo)

    def test_contains_outdated_adr(self):
        adr = read("docs", "adr-019-attribution.md")
        self.assertIn("30-day", adr)
        self.assertNotIn("superseded", adr.lower())  # the wiki must discover it

    def test_contains_scope_difference(self):
        prod = read("config", "analytics.prod.json")
        sandbox = read("config", "analytics.sandbox.json")
        self.assertIn('"click_lookback_days": 7', prod)
        self.assertIn('"click_lookback_days": 30', sandbox)

    def test_contains_conflicting_retry_values(self):
        adr = read("docs", "adr-007-retries.md")
        cfg = read("config", "analytics.prod.json")
        self.assertIn("3 times", adr)
        self.assertIn('"payment_gateway_retries": 5', cfg)

    def test_contains_failed_approach_scoped(self):
        note = read("notes", "failed-redis-pubsub.md")
        self.assertIn("REJECTED", note)
        self.assertIn("order-event-delivery", note)  # negative knowledge must be scoped
        self.assertIn("cache-invalidation", note)    # with boundary, not "never do this"

    def test_contains_derived_duplicate_for_independence(self):
        meeting = read("notes", "meeting-2026-09-10.md")
        adr = read("docs", "adr-019-attribution.md")
        self.assertIn("30-day", meeting)  # quotes the ADR: second reference, same origin
        self.assertIn("30-day", adr)

    def test_contains_unanswerable_question(self):
        self.assertIn("2028 pricing plan", read("questions.txt"))
        q = read("questions.txt")
        self.assertIn("dead-letter table", q)  # vocabulary-adjacent fabrication trap

    def test_contains_stale_note_trap_question(self):
        q = read("questions.txt")
        self.assertIn("latest doc say the attribution window", q)
        self.assertIn("7 days", q)  # expected answer pins the corrected value

    def test_contains_negative_knowledge_question(self):
        q = read("questions.txt")
        self.assertIn("Redis pub/sub for event durability", q)
        self.assertIn("cache-invalidation", q)  # expected answer keeps scope

    def test_contains_source_file_with_window_constant(self):
        src = read("services", "analytics", "attribution.py")
        self.assertIn("CLICK_LOOKBACK_DAYS = 7", src)
        self.assertIn("2026-09-01", src)

    def test_contains_pdf_with_correction_text(self):
        path = os.path.join(CORPUS, "docs", "spec-attribution-window.pdf")
        with open(path, "rb") as f:
            raw = f.read()
        self.assertTrue(raw.startswith(b"%PDF-"), "not a PDF: %s" % path)
        self.assertIn(b"7 days", raw)
        self.assertIn(b"2026-09-01", raw)


if __name__ == "__main__":
    unittest.main()
