#!/usr/bin/env python3
"""test_perf.py — unit tests for the rust-vs-python perf/install lane.

Covers: corpus generator determinism + size tiers, the percentile /
med-min-max / speedup helpers on hand-computed fixtures, and the real
install.sh failure modes executed inside a sandboxed HOME+PATH (tampered
.sha256 aborts before installing; missing tarball errors; --python
prints the dev-reference refusal).
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)

import gen_perf_corpus as corpus  # noqa: E402
import run_perf  # noqa: E402
import run_install_ux as ux  # noqa: E402

INSTALL_SH = os.path.join(REPO_ROOT, "install.sh")


class CorpusTests(unittest.TestCase):
    def test_determinism(self):
        a = corpus.gen_docs(25)
        b = corpus.gen_docs(25)
        self.assertEqual(a, b)
        with tempfile.TemporaryDirectory() as t1, \
                tempfile.TemporaryDirectory() as t2:
            corpus.write_corpus(t1, 25)
            corpus.write_corpus(t2, 25)
            for name, _ in a:
                with open(os.path.join(t1, name), "rb") as f:
                    x = f.read()
                with open(os.path.join(t2, name), "rb") as f:
                    y = f.read()
                self.assertEqual(x, y)

    def test_count_names_sizes(self):
        docs = corpus.gen_docs(150)
        self.assertEqual(len(docs), 150)
        names = [n for n, _ in docs]
        self.assertEqual(len(set(names)), 150)
        sizes = sorted(len(t.encode()) for _, t in docs)
        self.assertGreaterEqual(sizes[0], 400)
        self.assertLessEqual(sizes[-1], 23000)
        self.assertLess(sizes[0], 2000)     # small tier present
        self.assertGreater(sizes[-1], 8000)  # large tier present

    def test_facts_and_questions(self):
        qs = corpus.questions(120)
        self.assertEqual(len(qs), 120)
        self.assertEqual(len(set(qs)), 120)
        self.assertIn("svc-0000", qs[0])
        c = corpus.candidates_for("source_X", 1, 7)
        self.assertTrue(all(k["evidence"][0]["source_id"] == "source_X"
                            for k in c))
        self.assertEqual(c[0]["subject"], "svc-0007")
        self.assertEqual(c[0]["value"], corpus.owner_of(7))


class StatsTests(unittest.TestCase):
    def test_percentile(self):
        xs = list(range(1, 101))  # 1..100
        self.assertAlmostEqual(run_perf.percentile(xs, 50), 50.5)
        self.assertAlmostEqual(run_perf.percentile(xs, 0), 1)
        self.assertAlmostEqual(run_perf.percentile(xs, 100), 100)
        self.assertAlmostEqual(run_perf.percentile([5.0], 95), 5.0)
        with self.assertRaises(ValueError):
            run_perf.percentile([], 50)

    def test_med_min_max(self):
        s = run_perf.med_min_max([3.0, 1.0, 2.0, 4.0])
        self.assertEqual(s["median"], 2.5)
        self.assertEqual(s["min"], 1.0)
        self.assertEqual(s["max"], 4.0)
        self.assertEqual(s["n"], 4)
        with self.assertRaises(ValueError):
            run_perf.med_min_max([])

    def test_speedup(self):
        self.assertAlmostEqual(run_perf.speedup(10.0, 2.5), 4.0)
        self.assertIsNone(run_perf.speedup(None, 2.5))
        self.assertIsNone(run_perf.speedup(10.0, 0))
        self.assertIsNone(run_perf.speedup("n/a", 2.0))


class _SandboxCase(unittest.TestCase):
    """Shared sandbox: minimal PATH, fake HOME, real install.sh."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="rvs-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        os.makedirs(self.home)
        self.env = ux.sandbox_env(self.home)

    def run_install(self, extra_env=None):
        env = dict(self.env)
        if extra_env:
            env.update(extra_env)
        return subprocess.run(["/bin/bash", INSTALL_SH, "--binary"],
                              env=env, cwd=self.home, capture_output=True,
                              text=True, timeout=300)


class InstallFailureModeTests(_SandboxCase):
    @classmethod
    def setUpClass(cls):
        cls.tarball_dir = tempfile.mkdtemp(prefix="rvs-pkg-")
        cls.tarball, cls.sha = ux.build_tarball(cls.tarball_dir)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tarball_dir, True)

    def test_missing_tarball_clear_error(self):
        proc = self.run_install({
            "LLM_WIKI_BINARY_TARBALL": os.path.join(self.tmp, "nope.tgz")})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("not found", proc.stderr)
        self.assertFalse(os.path.exists(
            os.path.join(self.home, ".local", "bin", "llm-wiki")))

    def test_tampered_sha256_aborts(self):
        tdir = os.path.join(self.tmp, "tampered")
        os.makedirs(tdir)
        local = os.path.join(tdir, os.path.basename(self.tarball))
        shutil.copy2(self.tarball, local)
        with open(self.sha) as f:
            good = f.read().split()[0]
        bad = ("0" if good[0] != "0" else "1") + good[1:]
        with open(local + ".sha256", "w") as f:
            f.write("%s  %s\n" % (bad, os.path.basename(self.tarball)))
        proc = self.run_install({"LLM_WIKI_BINARY_TARBALL": local})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("sha256 mismatch", proc.stderr + proc.stdout)
        self.assertFalse(os.path.exists(
            os.path.join(self.home, ".local", "bin", "llm-wiki")))

    def test_good_sha256_passes_verification(self):
        proc = self.run_install({"LLM_WIKI_BINARY_TARBALL": self.tarball})
        self.assertEqual(proc.returncode, 0,
                         proc.stderr + proc.stdout[-2000:])
        self.assertTrue(os.path.exists(
            os.path.join(self.home, ".local", "bin", "llm-wiki")))
        self.assertIn("verified", proc.stdout)

    def test_sha256_sidecar_content(self):
        digest = hashlib.sha256()
        with open(self.tarball, "rb") as f:
            digest.update(f.read())
        with open(self.sha) as f:
            recorded = f.read().split()[0]
        self.assertEqual(digest.hexdigest(), recorded)

class PythonFlagTest(unittest.TestCase):
    def test_python_flag_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = ux.sandbox_env(tmp)
            proc = subprocess.run(["/bin/bash", INSTALL_SH, "--python"],
                                  env=env, cwd=tmp, capture_output=True,
                                  text=True, timeout=60)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("dev-reference", proc.stderr)
        self.assertIn("skill/llm-wiki/scripts", proc.stderr)


class SubcommandSurfaceTests(unittest.TestCase):
    def test_subcommand_counts(self):
        rust_bin = os.path.join(REPO_ROOT, "rust", "target", "release",
                                "llm-wiki")
        if not os.path.isfile(rust_bin):
            self.skipTest("rust binary not built")
        p = subprocess.run([rust_bin, "--help"], capture_output=True,
                           text=True)
        n = ux.count_subcommands(p.stdout)
        self.assertGreaterEqual(n, 25)
        sys.path.insert(0, os.path.join(HERE, ".."))
        import _engine as eng  # noqa: E402
        pp = subprocess.run(eng.engine_argv("python") + ["--help"],
                            capture_output=True, text=True)
        npy = ux.count_subcommands_argparse(pp.stdout)
        self.assertGreaterEqual(npy, 25)


if __name__ == "__main__":
    unittest.main()
