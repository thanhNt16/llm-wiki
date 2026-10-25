"""Regression tests for install.sh safety behavior.

Runs install.sh against a fake $HOME (never the real one). The script is
exercised in "checkout" mode (install.sh lives at the repo root next to
skill/), so no clone or network is needed. markitdown/uv/pipx/pip3 are
stubbed so the optional-extractor step never touches the network.
"""
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

SKILL_DIR = os.path.join(os.path.dirname(__file__), "..", "..")
REPO_ROOT = os.path.abspath(os.path.join(SKILL_DIR, ".."))
INSTALL = os.path.join(REPO_ROOT, "install.sh")
MARKER = ".llm-wiki-installed"
SKILL_NAMES = ["llm-wiki", "wiki-init", "wiki-ingest", "wiki-compile",
               "wiki-query", "wiki-context", "wiki-review", "wiki-doctor",
               "wiki-visualize", "wiki-write"]


def _real_dir(binary):
    path = shutil.which(binary)
    return os.path.dirname(path) if path else None


@unittest.skipUnless(os.path.isfile(INSTALL), "install.sh not present")
class InstallScriptTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="install-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        self.stubbin = os.path.join(self.tmp, "bin")
        os.makedirs(self.home)
        os.makedirs(self.stubbin)
        # Stubs: markitdown present (skips the whole installer chain), and
        # no-op uv/pipx/pip3 as belt-and-braces against real network installs.
        for name in ("markitdown", "uv", "pipx", "pip3"):
            stub = os.path.join(self.stubbin, name)
            with open(stub, "w") as f:
                f.write("#!/bin/sh\nexit 0\n")
            os.chmod(stub, stat.S_IRWXU)
        path_parts = [self.stubbin, "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        for d in (_real_dir("git"), _real_dir("python3")):
            if d and d not in path_parts:
                path_parts.append(d)
        self.env = dict(os.environ)
        self.env["HOME"] = self.home
        self.env["PATH"] = ":".join(path_parts)

    def run_install(self, *args):
        return subprocess.run(["bash", INSTALL] + list(args),
                              env=self.env, capture_output=True, text=True)

    def target(self, base, name):
        return os.path.join(self.home, base, "skills", name)

    # --- argument validation -------------------------------------------------

    def test_unknown_flag_rejected(self):
        r = self.run_install("--foo")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("usage:", r.stderr)
        self.assertFalse(os.path.exists(
            os.path.join(self.home, ".omp")), "nothing installed")

    def test_extra_args_rejected(self):
        r = self.run_install("--agents", "--cellockai")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("usage:", r.stderr)

    # --- symlink install + marker on copies ----------------------------------

    def test_symlink_install_and_uninstall(self):
        r = self.run_install()
        self.assertEqual(r.returncode, 0, r.stderr)
        link = self.target(".omp/agent", "wiki-init")
        self.assertTrue(os.path.islink(link))
        r = self.run_install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.lexists(link))

    def test_copy_install_writes_marker(self):
        r = self.run_install("--cellockai")
        self.assertEqual(r.returncode, 0, r.stderr)
        target = self.target(".cellockai", "wiki-init")
        self.assertTrue(os.path.isdir(target))
        self.assertFalse(os.path.islink(target))
        self.assertTrue(os.path.isfile(os.path.join(target, MARKER)))

    def test_copy_install_preserves_assets_dist(self):
        r = self.run_install("--cellockai")
        self.assertEqual(r.returncode, 0, r.stderr)
        dist = os.path.join(self.target(".cellockai", "wiki-visualize"),
                            "assets", "graph-ui", "dist")
        self.assertTrue(os.path.isdir(dist),
                        "shipped assets/graph-ui/dist must survive install")
        self.assertTrue(os.path.isfile(os.path.join(dist, "index.html")))
        # But junk dirs outside assets/ are still stripped.
        self.assertFalse(os.path.exists(os.path.join(
            self.target(".cellockai", "wiki-visualize"),
            "assets", "graph-ui", "node_modules")))

    # --- uninstall provenance -------------------------------------------------

    def test_uninstall_removes_marked_copy(self):
        r = self.run_install("--cellockai")
        self.assertEqual(r.returncode, 0, r.stderr)
        target = self.target(".cellockai", "wiki-init")
        self.assertTrue(os.path.isdir(target))
        r = self.run_install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(target))

    def test_uninstall_skips_unmarked_foreign_dir(self):
        # A user-authored same-named skill dir (SKILL.md but no marker).
        for base in (".omp/agent", ".agents", ".cellockai"):
            d = self.target(base, "wiki-init")
            os.makedirs(d)
            with open(os.path.join(d, "SKILL.md"), "w") as f:
                f.write("user-authored skill\n")
        r = self.run_install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("leaving it in place", r.stderr)
        for base in (".omp/agent", ".agents", ".cellockai"):
            d = self.target(base, "wiki-init")
            self.assertTrue(os.path.isfile(os.path.join(d, "SKILL.md")),
                            "%s must not be deleted" % d)

    def test_uninstall_only_removes_our_copies_mixed_case(self):
        # Marked copy in cellockai + foreign same-named dir under .agents:
        # marked removed, foreign preserved.
        marked = self.target(".cellockai", "wiki-init")
        os.makedirs(marked)
        with open(os.path.join(marked, "SKILL.md"), "w") as f:
            f.write("copy\n")
        with open(os.path.join(marked, MARKER), "w") as f:
            f.write("")
        foreign = self.target(".agents", "wiki-init")
        os.makedirs(foreign)
        with open(os.path.join(foreign, "SKILL.md"), "w") as f:
            f.write("mine\n")
        r = self.run_install("--uninstall")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(marked))
        self.assertTrue(os.path.isfile(os.path.join(foreign, "SKILL.md")))

    # --- clone provenance -----------------------------------------------------

    def _fake_clone(self, origin):
        clone = os.path.join(self.home, ".llm-wiki")
        os.makedirs(clone)
        subprocess.run(["git", "init"], cwd=clone, env=self.env,
                       capture_output=True, check=True)
        subprocess.run(["git", "remote", "add", "origin", origin],
                       cwd=clone, env=self.env, capture_output=True, check=True)
        return clone

    def test_foreign_clone_at_clone_home_rejected(self):
        # install.sh is invoked from a location without skill/ beside it, so
        # it falls back to the clone at $HOME/.llm-wiki — which is foreign.
        self._fake_clone("https://evil.example.com/not-llm-wiki.git")
        sandbox = os.path.join(self.tmp, "sandbox")
        os.makedirs(sandbox)
        script_copy = os.path.join(sandbox, "install.sh")
        shutil.copy(INSTALL, script_copy)
        r = subprocess.run(["bash", script_copy], env=self.env,
                           capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("refusing", r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.home, ".omp")))

    def test_purge_keeps_foreign_clone(self):
        clone = self._fake_clone("https://evil.example.com/not-llm-wiki.git")
        r = self.run_install("--purge")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(os.path.isdir(os.path.join(clone, ".git")))
        self.assertIn("leaving it in place", r.stderr)

    def test_purge_removes_own_clone(self):
        clone = self._fake_clone(
            "https://github.com/thanhNt16/llm-wiki.git")
        r = self.run_install("--purge")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(clone))


if __name__ == "__main__":
    unittest.main()
