"""SP3 Task 5: transaction.wait_for_lock — poll the commit lock until free."""
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import claims as _claims  # noqa: E402,F401  (import order: breaks txn circularity)
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import wait_for_lock  # noqa: E402

# holds an exclusive flock on the real commit lock file for argv[1] seconds
_HOLD_SNIPPET = (
    "import fcntl,sys,time;"
    "f=open(sys.argv[1],'a');"
    "fcntl.flock(f,fcntl.LOCK_EX);"
    "time.sleep(float(sys.argv[2]))"
)


class TestWaitForLock(unittest.TestCase):
    def setUp(self):
        self.w = Wiki(tempfile.mkdtemp(prefix="lockwait-"))
        init_wiki(self.w, "lockwait")
        self.lock_path = self.w.p(".state", "locks", "commit.lock")
        os.makedirs(os.path.dirname(self.lock_path), exist_ok=True)

    def tearDown(self):
        shutil.rmtree(self.w.root, ignore_errors=True)

    def _hold(self, seconds):
        return subprocess.Popen(
            [sys.executable, "-c", _HOLD_SNIPPET, self.lock_path, str(seconds)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def test_wait_for_lock_true_when_free(self):
        t0 = time.monotonic()
        self.assertTrue(wait_for_lock(self.w, 0.1))
        self.assertLess(time.monotonic() - t0, 1.0)

    def test_wait_blocks_until_release(self):
        proc = self._hold(1.5)
        try:
            time.sleep(0.4)  # let the holder take the flock
            t0 = time.monotonic()
            self.assertTrue(wait_for_lock(self.w, 5))
            self.assertGreaterEqual(time.monotonic() - t0, 1.0)
        finally:
            proc.wait()

    def test_wait_times_out(self):
        proc = self._hold(3.0)
        try:
            time.sleep(0.4)
            t0 = time.monotonic()
            self.assertFalse(wait_for_lock(self.w, 0.3))
            self.assertLess(time.monotonic() - t0, 2.5)
        finally:
            proc.kill()
            proc.wait()


if __name__ == "__main__":
    unittest.main()
