import os, unittest

from evals.run_benchmark import corpus_sources, candidates_path

CORPUS = os.path.join(os.path.dirname(__file__), "..", "golden-corpus")


class TestBenchmarkFixtures(unittest.TestCase):
    def test_every_source_has_candidates_file(self):
        for rel in corpus_sources(CORPUS):
            self.assertTrue(
                os.path.isfile(candidates_path(CORPUS, rel)),
                "missing candidates for %s" % rel)

    def test_candidates_parse_as_shorthand(self):
        import json
        for rel in corpus_sources(CORPUS):
            with open(candidates_path(CORPUS, rel)) as f:
                for n, line in enumerate(f, 1):
                    line = line.strip()
                    if not line:
                        continue
                    obj = json.loads(line)
                    self.assertIn("subject", obj, "%s line %d" % (rel, n))
                    self.assertIn("predicate", obj, "%s line %d" % (rel, n))
                    for banned in ("evidence", "recorded_at", "status", "root_origin"):
                        self.assertNotIn(banned, obj, "%s line %d" % (rel, n))

    def test_classifications_file_shape(self):
        import json
        path = os.path.join(CORPUS, "classifications.json")
        # relationships that consume target_claim_id at apply
        # (compile.py _TARGETED); SCOPE_DIFFERENCE/UNRELATED create a new
        # claim and carry no live-claim target
        targeted = ("CORRECTION", "POLICY_CHANGE", "CONTRADICTION",
                    "CORROBORATION", "DUPLICATE")
        for c in json.load(open(path)):
            self.assertIn(c["relationship"],
                          ("UNRELATED", "CORRECTION", "POLICY_CHANGE",
                           "SCOPE_DIFFERENCE", "CONTRADICTION",
                           "CORROBORATION", "DUPLICATE"))
            if c["relationship"] in targeted:
                self.assertIsNotNone(c.get("target"), c)
            else:
                # UNRELATED/SCOPE_DIFFERENCE create a new claim — a target
                # key would be dead fixture data
                self.assertNotIn("target", c, c)


if __name__ == "__main__":
    unittest.main()
