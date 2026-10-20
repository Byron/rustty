#!/usr/bin/env python3
"""Windows CLI selection, control negotiation and explicit owner election.

Legacy suite IDs are retained, with Windows namespace/explicit-start mechanisms.
All endpoints and owners are private. The only fake peer is a deliberately old
native protocol server used to prove that no unsupported command is submitted.
"""
from __future__ import annotations
import argparse
import ctypes
from ctypes import wintypes as w
import json
import msvcrt
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
import uuid
from windows_lifetime import Lifetimes, until
from windows_native_views import sha256
from windows_ssh import A, K, INVALID, Connection, identity, peek, raw_pipe_probe


class Lines:
    def __init__(self, child):
        self.child, self.buffer = child, bytearray()

    def wait(self, expected):
        deadline = time.monotonic() + 8
        while True:
            while b'\n' in self.buffer:
                line, _, self.buffer = self.buffer.partition(b'\n')
                if expected in line:
                    return line
            count = peek(self.child.stdout)
            assert count is not None, 'control client closed before ' + repr(expected)
            if count:
                self.buffer.extend(self.child.stdout.read(count))
            assert time.monotonic() < deadline, 'control record timeout: ' + repr(expected)
            time.sleep(.005)


class CLIState(Lifetimes):
    def endpoint(self, instance):
        return '\\\\.\\pipe\\tmuxx-' + identity()['account_sid'] + '-' + instance.encode('utf-8').hex()

    def run(self, *args, extra=None, success=True):
        output = subprocess.run([str(self.candidate), *args], capture_output=True,
                                env=self.env | (extra or {}), timeout=10,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        if success:
            assert output.returncode == 0, output.stderr
        else:
            assert output.returncode != 0 and output.stderr and not output.stdout, output
        return output

    def pipe_child(self, args, name):
        child = subprocess.Popen([str(self.candidate), *args], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env=self.env, bufsize=0, creationflags=subprocess.CREATE_NO_WINDOW)
        self.processes.append(child)
        return child

    def namespace(self):
        first, first_name = self.owner('caller-default')
        second, second_name = self.owner('label-\u03a9 &^ ,')
        self.run('-L', first_name, 'new-session', '-d', '-s', 'default', sys.executable, '-c', 'import time; time.sleep(180)')
        self.run('-L', second_name, 'new-session', '-d', '-s', 'named', sys.executable, '-c', 'import time; time.sleep(180)')
        first_path, second_path = self.endpoint(first_name), self.endpoint(second_name)
        assert first_path != second_path and raw_pipe_probe(first_path)['connected'] and raw_pipe_probe(second_path)['connected']
        inherited = {'TMUXX_ENDPOINT': first_path, 'TMUX': second_path + ',123,0'}
        assert self.run('list-sessions', '-F', '#{session_name}', extra=inherited).stdout == b'default\n'
        assert self.run('-L', second_name, 'list-sessions', '-F', '#{session_name}', extra=inherited).stdout == b'named\n'
        assert self.run('--native-endpoint', second_path, 'list-sessions', '-F', '#{session_name}', extra=inherited).stdout == b'named\n'
        self.run('--native-endpoint', first_path, '-L', second_name, 'list-sessions', success=False)
        self.run('-S', first_path, 'list-sessions', success=False)
        first_bridge, second_bridge = self.bridge(first_name), self.bridge(second_name)
        assert first_bridge.hello['instance_id'] != second_bridge.hello['instance_id']
        first_local, second_local = Connection.local(first_path), Connection.local(second_path)
        self.connections.extend([first_local, second_local])
        first_local.handshake(first_name, first_bridge.hello['instance_id'])
        second_local.handshake(second_name, second_bridge.hello['instance_id'])
        assert first_local.owner_pid == first.pid and second_local.owner_pid == second.pid
        for connection, expected in [(first_bridge, b'default\n'), (first_local, b'default\n'),
                                     (second_bridge, b'named\n'), (second_local, b'named\n')]:
            assert connection.request('list-sessions', '-F', '#{session_name}') == expected
        assert first_bridge.request('display', '-p', '#{pane_pid}') == first_local.request('display', '-p', '#{pane_pid}')
        assert second_bridge.request('display', '-p', '#{pane_pid}') == second_local.request('display', '-p', '#{pane_pid}')
        self.evidence['namespace'] = {'caller_default': first_path, 'explicit_label': second_path,
                                      'owner_pids': [first.pid, second.pid], 'TMUX_did_not_alias_native_selection': True}
        self.evidence['namespace']['incarnations'] = [first_bridge.hello['instance_id'], second_bridge.hello['instance_id']]
        self.evidence['namespace']['fixed_bridge_protocol_selection_matches_local_pipes'] = True
        for owner, instance in [(first, first_name), (second, second_name)]:
            pids = list(map(int, self.command(instance, 'list-panes', '-a', '-F', '#{pane_pid}').splitlines()))
            self.remember({'pane_processes': dict(enumerate(pids))})
            self.command(instance, 'kill-server')
            owner.wait(timeout=5)
            self.assert_dead(pids)
        self.checks.append('default_and_label_sockets_use_tmux_namespace_with_separate_native_endpoint')

    def control_wait(self):
        owner, instance = self.owner('control-wait')
        self.run('-L', instance, 'new-session', '-d', '-s', 'main', sys.executable, '-c', 'import time; time.sleep(180)')
        pid = int(self.command(instance, 'display', '-p', '#{pane_pid}'))
        self.remember({'pane_processes': {'pane': pid}})
        child = self.pipe_child(['--native-endpoint', self.endpoint(instance), '-C', 'attach-session', '-t', 'main'], 'control')
        lines = Lines(child)
        lines.wait(b'%end ')
        child.stdin.write(b'refresh-client -f wait-exit\n'); child.stdin.flush()
        lines.wait(b'%end ')
        child.stdin.write(b'detach-client\n'); child.stdin.flush()
        lines.wait(b'%exit')
        time.sleep(.05)
        assert child.poll() is None, 'control ignored wait-exit'
        child.stdin.write(b'\n'); child.stdin.flush()
        assert child.wait(timeout=5) == 0
        for file in [child.stdin, child.stdout, child.stderr]: file.close()
        assert self.alive(pid) and owner.poll() is None
        self.command(instance, 'kill-server'); owner.wait(timeout=5); self.assert_dead([pid])
        self.checks.append('explicit_native_control_wait_exit_observes_client_flags_and_newline')

    def old_capability(self):
        class Attributes(ctypes.Structure):
            _fields_ = [('length', w.DWORD), ('descriptor', w.LPVOID), ('inherit', w.BOOL)]
        A.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [w.LPCWSTR, w.DWORD, ctypes.POINTER(w.LPVOID), w.LPVOID]
        K.CreateNamedPipeW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD, ctypes.POINTER(Attributes)]
        K.CreateNamedPipeW.restype = w.HANDLE
        K.ConnectNamedPipe.argtypes = [w.HANDLE, w.LPVOID]
        descriptor = w.LPVOID()
        assert A.ConvertStringSecurityDescriptorToSecurityDescriptorW('D:P(A;;GA;;;' + identity()['account_sid'] + ')', 1, ctypes.byref(descriptor), None)
        attributes = Attributes(ctypes.sizeof(Attributes), descriptor, False)
        endpoint = self.endpoint('old-control-' + uuid.uuid4().hex)
        try:
            handle = K.CreateNamedPipeW(endpoint, 3 | 0x80000, 1 | 8, 1, 65536, 65536, 0, ctypes.byref(attributes))
            assert handle != INVALID
        finally:
            K.LocalFree(descriptor)
        file = os.fdopen(msvcrt.open_osfhandle(handle, os.O_RDWR | os.O_BINARY), 'r+b', buffering=0)
        peer = Connection(file, file)
        try:
            child = self.pipe_child(['--native-endpoint', endpoint, '-C', 'new-session', '-s', 'must-not-execute'], 'old-control')
            def connected():
                if K.ConnectNamedPipe(handle, None): return True
                error = ctypes.get_last_error()
                if error == 535: return True
                assert error == 536, error
                return False
            until(connected, 'old protocol control client did not connect')
            hello = peer.record()
            assert hello['control_commands'] is True
            peer.write({'version': 1, 'client_features': 0})
            assert child.wait(timeout=5) != 0
            stdout, stderr = child.stdout.read(), child.stderr.read()
            assert not stdout and b'server does not support control command framing' in stderr
            try:
                peer.record(timeout=1)
            except EOFError:
                pass
            else:
                raise AssertionError('client submitted a command without capability')
            for stream in [child.stdin, child.stdout, child.stderr]: stream.close()
        finally:
            peer.close()
        self.checks.append('native_control_rejects_old_servers_before_submitting_commands')

    def invalid(self):
        instance = 'invalid-' + uuid.uuid4().hex
        endpoint = self.endpoint(instance)
        env = {'TMUXX_ENDPOINT': endpoint}
        for flag in ['-h', '--help']:
            result = self.run(flag, extra=env)
            assert not result.stderr and b'Headless tmux-compatible' in result.stdout
        for args in [['-S'], ['--no-such-flag'], ['-v', 'new-session', '-d'], ['-l', 'new-session', '-d'], ['-c', 'echo never'], ['-D', 'new-session']]:
            self.run(*args, extra=env, success=False)
            assert not raw_pipe_probe(endpoint)['connected']
        assert self.run('-V', extra=env).stdout.startswith(b'tmuxx ')
        self.checks.append('clap_help_global_errors_and_excluded_modes_leave_no_server')
        file = self.directory / 'unrelated native endpoint \u03a9 &.txt'
        file.write_bytes(b'preserved\0\xff')
        failed = self.run('-D', '--native-endpoint', str(file), success=False)
        assert b'endpoint' in failed.stderr and file.read_bytes() == b'preserved\0\xff'
        assert not raw_pipe_probe(endpoint)['connected']
        self.checks.append('startup_failure_reports_cause_and_preserves_existing_path')
        self.run('-L', instance, 'new-session', '-d', ';', 'unknown-command', success=False)
        assert not raw_pipe_probe(endpoint)['connected']
        owner, _ = self.owner('registry', instance)
        assert self.run('-L', instance, 'list-comm', '-F', '#{command_list_name}', 'new').stdout == b'new-session\n'
        self.run('-L', instance, 'new-session', '-d', ';', 'unknown-command', success=False)
        assert not self.command(instance, 'list-sessions')
        self.command(instance, 'kill-server'); owner.wait(timeout=5)
        self.checks.append('startup_uses_all_validated_command_metadata_and_never_starts_invalid_groups')

    def election(self):
        old, instance = self.owner('election')
        before = self.bridge(instance)
        old_incarnation = before.hello['instance_id']
        before.close()
        self.command(instance, 'kill-server'); old.wait(timeout=5)
        contenders = [self.start([str(self.candidate), '-D', '-L', instance], 'contender-' + str(n)) for n in range(8)]
        until(lambda: sum(child.poll() is None for child in contenders) == 1, 'explicit owners did not elect exactly one')
        winner = next(child for child in contenders if child.poll() is None)
        assert all(child.returncode != 0 for child in contenders if child is not winner)
        after = self.bridge(instance)
        assert after.hello['instance_id'] != old_incarnation
        clients = [self.start([str(self.candidate), '-L', instance, 'new-session', '-d', '-s', 'client' + str(n),
                              sys.executable, '-c', 'import time; time.sleep(180)'], 'creation-' + str(n)) for n in range(8)]
        for child in clients: assert child.wait(timeout=10) == 0
        assert self.command(instance, 'list-sessions', '-F', '#{session_name}') == ''.join('client' + str(n) + '\n' for n in range(8))
        owner_pid = str(winner.pid) + '\n'
        assert self.command(instance, 'start', ';', 'display', '-p', '#{pid}') == owner_pid
        assert self.run('--native-endpoint', self.endpoint(instance), 'start-server', ';', 'display', '-p', '#{pid}').stdout.decode() == owner_pid
        pids = list(map(int, self.command(instance, 'list-panes', '-a', '-F', '#{pane_pid}').splitlines()))
        assert len(pids) == len(set(pids)) == 8
        self.remember({'pane_processes': dict(enumerate(pids))})
        self.evidence['election'] = {'contenders': [child.pid for child in contenders], 'winner': winner.pid,
                                     'pane_pids': pids, 'old_incarnation': old_incarnation, 'new_incarnation': after.hello['instance_id']}
        self.command(instance, 'kill-server'); winner.wait(timeout=5); self.assert_dead(pids)
        self.checks.append('startup_waits_for_a_closing_owner_lock_and_converges_concurrent_clients')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['gui', 'candidate', 'report']: parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'win32': parser.error('requires native Windows')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix='windows-cli-state-', dir=args.report.parent))
    fixture = CLIState(args.gui.resolve(strict=True), args.candidate.resolve(strict=True), directory)
    report = {'schema': 1, 'kind': 'windows-cli-state', 'artifact_directory': str(directory),
              'os': platform.platform(), 'architecture': platform.machine(),
              'gui_sha256': sha256(fixture.gui), 'candidate_sha256': sha256(fixture.candidate),
              'tests': [], 'failures': [], 'cleanup_remaining': []}
    try:
        fixture.namespace(); fixture.control_wait(); fixture.old_capability(); fixture.invalid(); fixture.election()
    except (Exception, KeyboardInterrupt) as error:
        report['failures'].append(f'{type(error).__name__}: {error}')
    finally:
        report['tests'], report['evidence'] = fixture.checks, fixture.evidence
        report['cleanup_remaining'] = fixture.cleanup()
        report['passed'] = not report['failures'] and not report['cleanup_remaining']
        args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(report))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
