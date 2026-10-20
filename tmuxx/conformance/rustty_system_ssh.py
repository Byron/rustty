#!/usr/bin/env python3
"""Run Rustty's actual system-tmux view test through an isolated loopback sshd."""
import argparse
from contextlib import contextmanager
import ctypes
import hashlib
import json
import os
from pathlib import Path
import pwd
import platform
import shlex
import shutil
import signal
import struct
import socket
import subprocess
import tempfile
import sys
import time



def process_identity(pid):
    """Identify the real owner, independently of the binary selected for clients."""
    if sys.platform == 'linux':
        process = Path('/proc') / str(pid)
        fields = (process / 'stat').read_text().rsplit(') ', 1)[1].split()
        if fields[0] == 'Z':
            raise RuntimeError('reference server exited')
        executable = (process / 'exe').resolve(strict=True)
        started = fields[19]
    elif sys.platform == 'darwin':
        libproc = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
        buffer = ctypes.create_string_buffer(4096)
        if libproc.proc_pidpath(pid, buffer, len(buffer)) <= 0:
            raise OSError(ctypes.get_errno(), 'proc_pidpath')
        executable = Path(os.fsdecode(buffer.value)).resolve(strict=True)
        started = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart='],
                                         text=True, timeout=3).strip()
        if not started:
            raise RuntimeError('reference server exited')
    else:
        raise RuntimeError('actual SSH reference identity requires Linux or macOS')
    return {'pid': pid, 'executable': str(executable), 'start_identity': started,
            'executable_sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}


def server_identity(reference, sock):
    raw = subprocess.check_output([str(reference), '-N', '-f', '/dev/null', '-S', str(sock),
                                   'display-message', '-p', '#{pid}'], timeout=5)
    pid = int(raw.strip())
    if sys.platform == 'linux':
        with socket.socket(socket.AF_UNIX) as peer:
            peer.settimeout(2)
            peer.connect(str(sock))
            actual, uid, _ = struct.unpack('3i', peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if (actual, uid) != (pid, os.getuid()):
                raise RuntimeError('C socket peer does not match its reported owner/user')
    identity = process_identity(pid)
    if identity['executable'] != str(reference):
        raise RuntimeError(f'wrong C server executable: {identity}')
    return identity


def verify_server(reference, sock, expected):
    actual = server_identity(reference, sock)
    if actual != expected:
        raise RuntimeError(f'C server incarnation changed: {expected} -> {actual}')
    return actual


def stop_reference(expected):
    """Terminate only the recorded owner, even if its socket path was replaced."""
    if not expected:
        return
    descriptor = None
    try:
        if sys.platform == 'linux':
            descriptor = os.pidfd_open(expected['pid'])
        if process_identity(expected['pid']) != expected:
            return
        if descriptor is not None:
            signal.pidfd_send_signal(descriptor, signal.SIGTERM)
        else:
            os.kill(expected['pid'], signal.SIGTERM)
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                if process_identity(expected['pid']) != expected:
                    return
            except (OSError, RuntimeError):
                return
            time.sleep(.02)
        if descriptor is not None:
            signal.pidfd_send_signal(descriptor, signal.SIGKILL)
        elif process_identity(expected['pid']) == expected:
            os.kill(expected['pid'], signal.SIGKILL)
    except (OSError, RuntimeError):
        pass
    finally:
        if descriptor is not None:
            os.close(descriptor)


@contextmanager
def fixture_directory():
    root = Path(tempfile.mkdtemp(prefix='rustty-system-ssh-'))
    try:
        yield root
    except BaseException:
        print(f'Rustty SSH failure transcripts retained in {root}', file=sys.stderr)
        raise
    else:
        shutil.rmtree(root)


def run():
    parser = argparse.ArgumentParser()
    parser.add_argument('--rustty', required=True, type=Path)
    parser.add_argument('--reference', required=True, type=Path)
    parser.add_argument('--output', type=Path, help='retain JSON evidence and the Cargo log')
    parser.add_argument('--sshd', default='/usr/sbin/sshd')
    parser.add_argument('--ssh', default='/usr/bin/ssh')
    parser.add_argument('--ssh-keygen', default='/usr/bin/ssh-keygen')
    args = parser.parse_args()
    reference = args.reference.resolve()
    with fixture_directory() as root:
        for name in ('host', 'client'):
            subprocess.run([args.ssh_keygen, '-q', '-t', 'ed25519', '-N', '', '-f', str(root / name)], check=True, timeout=10)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        user = pwd.getpwuid(os.getuid()).pw_name
        sock = root / 'tmux'
        dispatch = root / 'dispatch'
        dispatch.write_text('#!/usr/bin/env python3\nimport json, os, shlex, sys\n'
                            f'sys.path.insert(0, {str(Path(__file__).resolve().parent)!r})\n'
                            'from pathlib import Path\nfrom rustty_system_ssh import verify_server\n'
                            'argv=shlex.split(os.environ.get("SSH_ORIGINAL_COMMAND", ""))\n'
                            'if argv[:2] != ["exec", "tmux"]: sys.exit(1)\n'
                            f'verify_server(Path({str(reference)!r}), Path({str(sock)!r}), json.loads(Path({str(root / "owner.json")!r}).read_text()))\n'
                            f'os.execv({str(reference)!r}, [{str(reference)!r}, "-N", "-f", "/dev/null", "-S", {str(sock)!r}] + argv[2:])\n')
        dispatch.chmod(0o700)
        public = (root / 'client.pub').read_text()
        (root / 'authorized_keys').write_text(f'restrict,command="{dispatch}" {public}')
        (root / 'sshd.conf').write_text(
            f'Port {port}\nListenAddress 127.0.0.1\nHostKey {root}/host\nPidFile {root}/pid\n'
            f'AuthorizedKeysFile {root}/authorized_keys\nAllowUsers {user}\n'
            'PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n'
            'StrictModes no\nLogLevel ERROR\nPermitTTY no\nAllowTcpForwarding no\nX11Forwarding no\n')
        host_key = (root / 'host.pub').read_text().split()
        (root / 'known_hosts').write_text(f'[127.0.0.1]:{port} {host_key[0]} {host_key[1]}\n')
        (root / 'ssh.conf').write_text(f'Host isolated-tmux\n HostName 127.0.0.1\n Port {port}\n User {user}\n'
            f' IdentityFile {root}/client\n IdentitiesOnly yes\n BatchMode yes\n StrictHostKeyChecking yes\n'
            f' UserKnownHostsFile {root}/known_hosts\n GlobalKnownHostsFile /dev/null\n ControlMaster no\n ConnectTimeout 2\n')
        (root / 'ssh').write_text(f'#!/bin/sh\nexec {shlex.quote(args.ssh)} -F {shlex.quote(str(root / "ssh.conf"))} "$@"\n')
        (root / 'ssh').chmod(0o700)
        with (root / 'sshd.log').open('wb') as log:
            daemon = subprocess.Popen([args.sshd, '-D', '-e', '-f', str(root / 'sshd.conf')], stderr=log)
            command = [str(reference), '-f', '/dev/null', '-S', str(sock)]
            owner = None
            try:
                # Exactly one explicit startup. All later local/SSH clients refuse
                # autostart, and every forced SSH dispatch verifies the same owner.
                subprocess.run(command + ['new-session', '-d', '-s', 'one', '-x', '80', '-y', '24', '/bin/sh'], check=True, timeout=5)
                command.insert(1, '-N')
                owner = server_identity(reference, sock)
                (root / 'owner.json').write_text(json.dumps(owner))
                subprocess.run(command + ['new-session', '-d', '-s', 'two', '-x', '80', '-y', '24', '/bin/sh'], check=True, timeout=5)
                subprocess.run(command + ['split-window', '-d', '-t', 'one:0', '/bin/sh'], check=True, timeout=5)
                subprocess.run(command + ['link-window', '-s', 'one:0', '-t', 'two:1'], check=True, timeout=5)
                usage = subprocess.check_output(command + ['list-commands'], timeout=5).decode()
                paste_usage = next(line for line in usage.splitlines() if line.startswith('paste-buffer '))
                assert 'S' in paste_usage and 'r' in paste_usage and 'd' in paste_usage, paste_usage
                subprocess.run(command + ['set-buffer', '-b', 'user-kept', 'sentinel'], check=True, timeout=5)
                deadline = time.monotonic() + 5
                while True:
                    probe = subprocess.run([str(root / 'ssh'), '-T', 'isolated-tmux', 'exec tmux has-session -t one'], capture_output=True, timeout=5)
                    if probe.returncode == 0:
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError(probe.stderr.decode(errors='replace') + (root / 'sshd.log').read_text())
                    time.sleep(.05)
                env = dict(os.environ, RUSTTY_TEST_SSH_HOST='isolated-tmux', PATH=str(root) + os.pathsep + os.environ['PATH'])
                get_clipboard = subprocess.run(command + ['show-options', '-s', 'get-clipboard'], capture_output=True, timeout=5).returncode == 0
                if get_clipboard:
                    env['RUSTTY_TEST_GET_CLIPBOARD'] = '1'
                else:
                    env.pop('RUSTTY_TEST_GET_CLIPBOARD', None)
                floating_usage = next((line for line in usage.splitlines() if line.startswith('new-pane ')), '')
                floating = bool(floating_usage)
                modal = 'O' in floating_usage.partition('[')[2].partition(']')[0]
                if floating:
                    env['RUSTTY_TEST_FLOATING'] = '1'
                else:
                    env.pop('RUSTTY_TEST_FLOATING', None)
                if modal:
                    env['RUSTTY_TEST_MODAL'] = '1'
                else:
                    env.pop('RUSTTY_TEST_MODAL', None)
                source_names = ('crates/rustty/src/tmuxx.rs', 'crates/rustty/src/tmuxx/system.rs',
                                'crates/rustty/src/session.rs', 'crates/rustty-app/src/tmuxx_workspace.rs')
                sources_before = {name: hashlib.sha256((args.rustty / name).read_bytes()).hexdigest()
                                  for name in source_names}
                revision_before = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=args.rustty,
                                                           text=True, timeout=5).strip()
                from runner import run_suite_process
                try:
                    test = run_suite_process(['cargo', '+1.95.0', 'test', '--locked', '--offline', '-p', 'rustty', '--features', 'sessions',
                        'tmuxx::system::tests::stock_ssh_views_share_live_panes_and_survive_connection_restart',
                        '--', '--ignored', '--test-threads=1'], cwd=args.rustty, env=env, timeout=120)
                except subprocess.TimeoutExpired as error:
                    transcript = (error.stdout or b'') + (error.stderr or b'')
                    (root / 'cargo.log').write_bytes(transcript)
                    if args.output:
                        args.output.parent.mkdir(parents=True, exist_ok=True)
                        args.output.with_suffix('.log').write_bytes(transcript)
                    raise
                test.stdout = test.stdout.decode('utf-8', 'replace')
                test.stderr = test.stderr.decode('utf-8', 'replace')
                (root / 'cargo.log').write_text(test.stdout + test.stderr)
                verified_owner = verify_server(reference, sock, owner)
                sources_after = {name: hashlib.sha256((args.rustty / name).read_bytes()).hexdigest()
                                 for name in source_names}
                revision_after = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=args.rustty,
                                                          text=True, timeout=5).strip()
                if sources_before != sources_after or revision_before != revision_after:
                    raise RuntimeError('Rustty sources changed during SSH evidence collection')
                report = {'reference': str(reference), 'version': subprocess.check_output([str(reference), '-V'], text=True, timeout=5).strip(),
                    'server_identity': verified_owner, 'reference_sha256': hashlib.sha256(reference.read_bytes()).hexdigest(), 'exit_code': test.returncode,
                    'status': 'failed', 'rustty_sources': sources_after, 'rustty_revision': revision_after,
                    'host': {'platform': sys.platform, 'kernel': platform.release(), 'architecture': platform.machine(),
                             'libc': list(platform.libc_ver()), 'python': sys.version},
                    'assertions': ['shared-pane-independent-selection', 'live-input-output', 'dynamic-pane-create-remove',
                        'reconnect-capture', 'same-session-duplicate-winlinks-and-active-index',
                        'mouse-and-protocol-reply-not-synchronized', 'ordinary-text-synchronized', 'nul-rejected',
                        'temporary-buffers-cleaned-on-success-and-missing-target', 'existing-user-buffer-preserved', 'deferred-server-text-forwarding',
                        'c-clipboard-buffer-replies-once-with-no-mirror-host-action' if get_clipboard else 'c-clipboard-query-silence-with-no-mirror-host-action']}
                if floating:
                    report['assertions'] += ['new-layouts-signed-floating-geometry']
                if modal:
                    report['assertions'] += ['modal-pane-discovery']
                if args.output:
                    args.output.parent.mkdir(parents=True, exist_ok=True)
                    args.output.with_suffix('.log').write_text(test.stdout + test.stderr)
                    args.output.write_text(json.dumps(report, indent=2) + '\n')
                # Preserve exact Cargo test names for the conformance gate.
                print(test.stdout, end='')
                if test.returncode:
                    print(test.stderr)
                test.check_returncode()
                buffers = subprocess.check_output(command + ['list-buffers', '-F', '#{buffer_name}'], timeout=5)
                assert b'__rustty_input_' not in buffers, buffers
                assert subprocess.check_output(command + ['show-buffer', '-b', 'user-kept'], timeout=5) == b'sentinel'
                report['status'] = 'passed'
                if args.output:
                    args.output.write_text(json.dumps(report, indent=2) + '\n')
                print(json.dumps({'reference': report['version'], 'status': report['status'], 'assertions': len(report['assertions'])}))
            finally:
                stop_reference(owner)
                daemon.terminate()
                try:
                    daemon.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()


if __name__ == '__main__':
    run()
