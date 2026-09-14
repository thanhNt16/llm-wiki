"""Unittest wrapper so `python3 -m unittest discover` includes release gates."""
import os, sys, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
import importlib.util

spec = importlib.util.spec_from_file_location(
    "run_gates", os.path.join(HERE, "run_gates.py"))
run_gates = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run_gates)


class TestReleaseGates(unittest.TestCase):
    pass


def _make(name, fn):
    def test(self):
        ok, detail = fn()
        self.assertTrue(ok, "%s: %s" % (name, detail))
    test.__doc__ = "%s gate" % name
    return test


for _name, _fn in run_gates.GATES.items():
    setattr(TestReleaseGates, "test_gate_%s" % _name, _make(_name, _fn))

if __name__ == "__main__":
    unittest.main()
