"""SP3-T8 (#21): golden-corpus table/rationale/acronym extraction fixture.

``evals/golden-corpus/table-rationale.md`` exercises the three extraction
rules documented in ``skill/llm-wiki/references/EXTRACTION.md`` (section
"Tables, rationale clauses, acronyms"):

- one ``equivalence`` claim per row of an equivalence-map table,
  located by ``l:<row line>``;
- one ``rationale`` entry citing the fixture's because-sentence verbatim
  via an ``s:<exact text>`` locator;
- one ``expansion`` entry keeping the first expansion of an acronym.

The expected candidates must survive ``normalize_candidate``'s T2 field
allowlist — no ``quote`` key, no dead fields.
"""
import json
import os
import re
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
sys.path.insert(0, os.path.join(REPO, "skill", "llm-wiki", "scripts"))

from wikicore import claims  # noqa: E402,F401 -- import first: breaks the
# transaction<->ftx<->claims cycle that shorthand alone would trip over.
from wikicore.shorthand import normalize_candidate  # noqa: E402

CORPUS = os.path.join(REPO, "evals", "golden-corpus")
FIXTURE = os.path.join(CORPUS, "table-rationale.md")
EXPECTED = os.path.join(CORPUS, "table-rationale.expected.jsonl")


def _load_candidates():
    cands = []
    with open(EXPECTED) as fh:
        for line in fh:
            if line.strip():
                cands.append(json.loads(line))
    return cands


def _fixture_lines():
    with open(FIXTURE) as fh:
        return fh.read().splitlines()


def _because_sentence():
    """The sentence in the fixture containing 'because' (single line)."""
    for line in _fixture_lines():
        if "because" in line:
            return line.strip()
    return ""


def _data_rows():
    """1-based line numbers of markdown table *data* rows: pipe lines that
    are neither the header (first pipe line of a contiguous table block)
    nor a --- separator row."""
    lines = _fixture_lines()
    rows, in_table = [], False
    for i, line in enumerate(lines, 1):
        if line.count("|") < 2:
            in_table = False
            continue
        if re.match(r"^\s*\|[\s\-|]+\|\s*$", line):
            continue  # separator row
        if in_table:
            rows.append(i)
        else:
            in_table = True  # first pipe line opens the table = header
    return rows


def _locator_value(cand):
    """(locator text, shorthand kind 'h'/'l'/'s') for a candidate."""
    loc = cand.get("locator", "")
    m = re.match(r"^([hls]):(.*)$", loc, re.S)
    return (m.group(2), m.group(1)) if m else (loc, "")


class TestExtractionFixture(unittest.TestCase):
    def test_every_candidate_normalizes(self):
        """(a) every expected.jsonl line survives normalize_candidate."""
        cands = _load_candidates()
        self.assertGreater(len(cands), 0, "expected.jsonl is empty/missing")
        for i, cand in enumerate(cands, 1):
            out = normalize_candidate(dict(cand), "fixture", 1, "fixture")
            self.assertIsInstance(out, dict, "line %d" % i)
            self.assertTrue(out["subject"])
            self.assertTrue(out["predicate"])

    def test_one_equivalence_per_table_row(self):
        """(b) exactly one equivalence candidate per fixture table row,
        each located by the row's own l:<line> locator."""
        cands = _load_candidates()
        eq = [c for c in cands if c.get("predicate") == "equivalence"]
        data_rows = _data_rows()
        self.assertEqual(len(eq), 3)
        self.assertEqual(len(eq), len(data_rows),
                         "fixture must have one data row per equivalence")
        lines = _fixture_lines()
        for c in eq:
            text, kind = _locator_value(c)
            self.assertEqual(kind, "l", "equivalence rows use l:<line>")
            m = re.match(r"^(\d+)(?:-\d+)?$", text)
            self.assertIsNotNone(m, "bad l: locator %r" % c["locator"])
            row = lines[int(m.group(1)) - 1]
            self.assertIn(c["subject"], row)
            self.assertIn(str(c["value"]), row)

    def test_one_rationale_quoting_because_sentence(self):
        """(c) exactly one rationale entry whose s: locator is an exact
        substring of the fixture's because-sentence."""
        rat = [c for c in _load_candidates()
               if c.get("predicate") == "rationale"]
        self.assertEqual(len(rat), 1)
        text, kind = _locator_value(rat[0])
        self.assertEqual(kind, "s", "rationale cites verbatim text")
        sentence = _because_sentence()
        self.assertTrue(sentence, "fixture has no because-sentence")
        self.assertIn(text, sentence)

    def test_one_mwaa_expansion(self):
        """(d) exactly one expansion entry: MWAA -> Managed Workflows
        for Apache Airflow."""
        exp = [c for c in _load_candidates()
               if c.get("predicate") == "expansion"]
        self.assertEqual(len(exp), 1)
        self.assertEqual(exp[0]["subject"], "MWAA")
        self.assertEqual(exp[0]["value"],
                         "Managed Workflows for Apache Airflow")


if __name__ == "__main__":
    unittest.main()
