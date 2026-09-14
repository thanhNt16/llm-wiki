import os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import ids, hashing, yamlite


class TestIds(unittest.TestCase):
    def test_new_format_and_uniqueness(self):
        a, b = ids.new("claim"), ids.new("claim")
        self.assertTrue(a.startswith("claim_"))
        self.assertNotEqual(a, b)
        self.assertEqual(len(a), len("claim_") + 26)

    def test_validate(self):
        self.assertTrue(ids.validate(ids.new("source")))
        self.assertFalse(ids.validate("nope"))
        self.assertFalse(ids.validate("claim_!!bad"))

    def test_kinds(self):
        for kind in ("claim", "decision", "source", "context", "run", "review"):
            self.assertTrue(ids.validate(ids.new(kind)))


class TestHashing(unittest.TestCase):
    def test_canonical_json_sorted(self):
        self.assertEqual(hashing.canonical_json({"b": 1, "a": 2}), '{"a":2,"b":1}')

    def test_sha256_file(self):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(b"hello")
            p = f.name
        try:
            self.assertEqual(hashing.sha256_file(p), hashing.sha256_bytes(b"hello"))
        finally:
            os.unlink(p)

    def test_known_vector(self):
        self.assertEqual(
            hashing.sha256_bytes(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        )


class TestYamlite(unittest.TestCase):
    def test_round_trip(self):
        d = {
            "status": "accepted",
            "count": 3,
            "ratio": 0.5,
            "ok": True,
            "note": None,
            "tags": ["a", "b"],
        }
        text = "---\n" + yamlite.dumps(d) + "\n---\n"
        self.assertEqual(yamlite.loads(text), d)

    def test_rejects_nested(self):
        with self.assertRaises(ValueError):
            yamlite.dumps({"a": {"b": 1}})

    def test_scalars_with_colon_and_hash(self):
        d = {"t": "a: b # c"}
        self.assertEqual(yamlite.loads("---\n" + yamlite.dumps(d) + "\n---"), d)

    def test_string_list_round_trip(self):
        d = {"deps": ["claim_01JABC@3", "decision_01JDEF@2"]}
        text = "---\n" + yamlite.dumps(d) + "\n---\n"
        self.assertEqual(yamlite.loads(text), d)

    def test_empty_and_multiline_fence(self):
        self.assertEqual(yamlite.loads("---\n---"), {})
        self.assertEqual(yamlite.loads("no fences here"), {})


if __name__ == "__main__":
    unittest.main()
