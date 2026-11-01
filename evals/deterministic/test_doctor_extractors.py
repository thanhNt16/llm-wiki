"""SP3 Task 5: doctor reports markitdown extractor capability."""
import os
import shutil
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import doctor  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402

_FORMATS = ("application/pdf", ".docx", ".xlsx")


class TestDoctorExtractors(unittest.TestCase):
    def setUp(self):
        self.w = Wiki(tempfile.mkdtemp(prefix="docext-"))
        init_wiki(self.w, "docext")

    def tearDown(self):
        shutil.rmtree(self.w.root, ignore_errors=True)
        sys.modules.pop("markitdown", None)

    def _stub_markitdown(self):
        """Directory with a stub markitdown.py (module present on sys.path)."""
        d = tempfile.mkdtemp(prefix="mkstub-")
        self.addCleanup(shutil.rmtree, d, True)
        with open(os.path.join(d, "markitdown.py"), "w") as f:
            f.write("# stub\n")
        return d

    def test_extractors_report_module_available(self):
        stub = self._stub_markitdown()
        sys.path.insert(0, stub)
        self.addCleanup(sys.path.remove, stub)
        r = doctor.run(self.w)["extractors"]["markitdown"]
        self.assertTrue(r["available"])
        self.assertEqual(r["via"], "module")
        for fmt in _FORMATS:
            self.assertEqual(r["formats"][fmt], "extracted")
        self.assertIsNone(r["remediation"])

    def test_extractors_report_cli_available(self):
        # no module on sys.path, but a markitdown executable exists; pin
        # find_spec too — a machine with markitdown installed would
        # otherwise report via="module" (SP3-F5 T5-S1)
        with mock.patch("importlib.util.find_spec", return_value=None), \
                mock.patch("shutil.which",
                           return_value="/usr/local/bin/markitdown"):
            r = doctor.run(self.w)["extractors"]["markitdown"]
        self.assertTrue(r["available"])
        self.assertEqual(r["via"], "cli")
        for fmt in _FORMATS:
            self.assertEqual(r["formats"][fmt], "extracted")
        self.assertIsNone(r["remediation"])

    def test_extractors_report_unavailable(self):
        # env may have the module installed — pin both probes to "absent"
        with mock.patch("importlib.util.find_spec", return_value=None), \
                mock.patch("shutil.which", return_value=None):
            r = doctor.run(self.w)["extractors"]["markitdown"]
        self.assertFalse(r["available"])
        self.assertIsNone(r["via"])
        for fmt in _FORMATS:
            self.assertEqual(r["formats"][fmt], "agent-handoff")
        self.assertEqual(r["remediation"], "pip install markitdown (optional)")


if __name__ == "__main__":
    unittest.main()
