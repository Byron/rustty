"""Linux C-fixture process regressions; these do not qualify Windows behavior."""
from contextlib import contextmanager
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import remote_c_reference as oracle
import upstream_reference as upstream


@unittest.skipUnless(sys.platform.startswith("linux"), "requires the Linux C-reference subreaper")
class UpstreamOwnershipTests(unittest.TestCase):
    @contextmanager
    def fixture(self):
        # This unittest process owns only its private fixtures. Cleanup runs on
        # assertion failure, timeout and interruption as well as success.
        oracle.enable_descendant_owner()
        with tempfile.TemporaryDirectory(prefix="tx-upstream-reaping-") as temporary:
            root = Path(temporary)
            (root / "regress").mkdir()
            try:
                yield root
            finally:
                self.assertEqual(oracle.reclaim_owned_children(), [])

    def assert_gone(self, pid):
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_reaps_orphan_preserving_script_status_and_live_descendant(self):
        with self.fixture() as root:
            marker = root / "descendants"
            child = root / "orphan.py"
            child.write_text("""import os,sys,time
from pathlib import Path
dead=os.fork()
if dead == 0: os._exit(0)
live=os.fork()
if live == 0:
    time.sleep(60)
    os._exit(0)
Path(sys.argv[1]).write_text(f'{dead} {live}')
os._exit(0)
""", encoding="utf-8")
            script = root / "regress" / "adopted-exit.sh"
            script.write_text(
                shlex.join([sys.executable, str(child), str(marker)]) + "\n"
                + "read dead live < " + shlex.quote(str(marker)) + "\n"
                + "n=0\nwhile kill -0 \"$dead\" 2>/dev/null; do\n"
                + "  [ $n -lt 60 ] || exit 85\n  n=$((n + 1))\n  sleep .05\ndone\n"
                + "kill -0 \"$live\" || exit 86\necho orphan-reaped\nexit 37\n",
                encoding="utf-8",
            )
            with patch.dict(upstream.os.environ, {"TMUXX_C_SOURCE": str(root)}), \
                 patch.object(upstream, "SCRIPTS", (script.name,)):
                result = upstream.upstream(Path("/unused-reference"), root, script.name)
            self.assertEqual(result["returncode"], 37, "an exited adopted process still satisfied kill -0")
            dead, live = map(int, marker.read_text().split())
            self.assert_gone(dead)
            self.assertEqual(os.waitpid(live, os.WNOHANG), (0, 0), "live adopted child was killed or reaped")

    def waiting_script(self, root):
        script = root / "regress" / "waiting.sh"
        script.write_text(
            "echo $$ > " + shlex.quote(str(root / "shell.pid")) + "\n"
            + "sleep 60 &\necho $! > " + shlex.quote(str(root / "live.pid")) + "\nwait\n",
            encoding="utf-8",
        )
        return script

    def assert_script_closed_and_descendant_live(self, root):
        self.assert_gone(int((root / "shell.pid").read_text()))
        pid = int((root / "live.pid").read_text())
        self.assertEqual(os.waitpid(pid, os.WNOHANG), (0, 0))

    def test_timeout_reaps_script_and_leaves_live_descendant_for_owner_cleanup(self):
        with self.fixture() as root, (root / "output").open("wb") as output:
            script = self.waiting_script(root)
            with self.assertRaises(subprocess.TimeoutExpired):
                upstream.run_script(script, os.environ.copy(), output, timeout=.5)
            self.assert_script_closed_and_descendant_live(root)

    def test_interruption_reaps_script_and_leaves_live_descendant_for_owner_cleanup(self):
        class Interrupted(Exception):
            pass

        def interrupt(*_):
            raise Interrupted("private fixture interruption")

        with self.fixture() as root, (root / "output").open("wb") as output:
            script = self.waiting_script(root)
            old = signal.signal(signal.SIGALRM, interrupt)
            try:
                signal.setitimer(signal.ITIMER_REAL, .5)
                with self.assertRaises(Interrupted):
                    upstream.run_script(script, os.environ.copy(), output, timeout=5)
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)
                signal.signal(signal.SIGALRM, old)
            self.assert_script_closed_and_descendant_live(root)


if __name__ == "__main__":
    unittest.main()
