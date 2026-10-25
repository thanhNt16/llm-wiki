"""Smoke tests for the bench harness — end-to-end against the HEAD engine.

Each test runs bench.py as a subprocess (full wiki build + probes + metrics,
~30-90s); keep the 600s timeout. No sibling-module imports, so the file works
both under `python3 -m unittest discover -s evals/benchmarks -p "test_*.py"`
and package-style (`python3 -m unittest evals.benchmarks.test_bench`)."""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
ENGINE = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")


class BenchSmokeTest(unittest.TestCase):
    def test_bench_runs_and_emits_valid_json(self):
        out_dir = tempfile.mkdtemp(prefix="bench-out-")
        try:
            proc = subprocess.run(
                [sys.executable, os.path.join(HERE, "bench.py"),
                 "--engine", ENGINE, "--label", "smoke", "--out", out_dir],
                capture_output=True, text=True, timeout=600, cwd=HERE)
            self.assertEqual(proc.returncode, 0, proc.stderr[-500:])
            path = os.path.join(out_dir, "bench-smoke.json")
            self.assertTrue(os.path.isfile(path))
            with open(path) as f:
                data = json.load(f)
            for section in ("retrieval", "context", "topology", "hygiene",
                            "guard", "twosource"):
                self.assertIn(section, data)
            self.assertTrue(data["build_steps"])
        finally:
            shutil.rmtree(out_dir, ignore_errors=True)

    def test_bench_deterministic(self):
        outs = []
        for i in (1, 2):
            out_dir = tempfile.mkdtemp(prefix="bench-det-%d-" % i)
            try:
                subprocess.run(
                    [sys.executable, os.path.join(HERE, "bench.py"),
                     "--engine", ENGINE, "--label", "det%d" % i,
                     "--out", out_dir],
                    capture_output=True, text=True, timeout=600, cwd=HERE,
                    check=True)
                with open(os.path.join(out_dir, "bench-det%d.json" % i)) as f:
                    outs.append(json.load(f))
            finally:
                shutil.rmtree(out_dir, ignore_errors=True)
        a, b = outs
        for section in ("retrieval", "context", "topology", "guard",
                        "twosource", "hygiene"):
            self.assertEqual(a[section], b[section],
                             "non-deterministic metrics in %s" % section)


if __name__ == "__main__":
    unittest.main()
