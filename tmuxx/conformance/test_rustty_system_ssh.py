import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

if sys.platform in ('darwin', 'linux'):
    import rustty_system_ssh as fixture


@unittest.skipUnless(sys.platform in ('darwin', 'linux'), 'requires POSIX SSH fixture')
class CleanupTests(unittest.TestCase):
    def test_darwin_identity_only_normalizes_an_exited_process(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / 'tmux'
            executable.write_bytes(b'fixture')

            def process_path(pid, buffer, length):
                buffer.value = os.fsencode(executable)
                return len(buffer.value)

            libproc = Mock()
            libproc.proc_pidpath.side_effect = process_path
            for status, output, error, expected in [
                (1, '', '', ProcessLookupError),
                (2, '', '', subprocess.CalledProcessError),
                (1, '', 'permission denied', subprocess.CalledProcessError),
                (1, 'unexpected output', '', subprocess.CalledProcessError),
            ]:
                failure = subprocess.CalledProcessError(status, ['ps'], output, error)
                with self.subTest(status=status, output=output, error=error), \
                     patch.object(fixture.sys, 'platform', 'darwin'), \
                     patch.object(fixture.ctypes, 'CDLL', return_value=libproc), \
                     patch.object(fixture.subprocess, 'check_output', side_effect=failure), \
                     self.assertRaises(expected):
                    fixture.process_identity(123)

    def test_owner_exit_after_term_does_not_escalate(self):
        owner = {'pid': 123}
        with patch.object(fixture.sys, 'platform', 'darwin'), \
             patch.object(fixture, 'process_identity', side_effect=[owner, ProcessLookupError()]), \
             patch.object(fixture.os, 'kill') as kill:
            fixture.stop_reference(owner)
        kill.assert_called_once_with(123, signal.SIGTERM)

    def test_reference_cleanup_failure_still_closes_sshd(self):
        for timeout in [False, True]:
            daemon = Mock()
            if timeout:
                daemon.wait.side_effect = [subprocess.TimeoutExpired('sshd', 5), 0]
            failure = subprocess.CalledProcessError(2, ['ps'])
            with self.subTest(timeout=timeout), \
                 patch.object(fixture, 'stop_reference', side_effect=failure), \
                 self.assertRaises(subprocess.CalledProcessError):
                fixture.stop_fixture({'pid': 123}, daemon)
            daemon.terminate.assert_called_once_with()
            daemon.wait.assert_any_call(timeout=5)
            if timeout:
                daemon.kill.assert_called_once_with()
                daemon.wait.assert_called_with()
            else:
                daemon.kill.assert_not_called()
