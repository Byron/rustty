#!/usr/bin/env python3
"""Native Windows counterparts of the shared CLI lifecycle obligations.

Uses packaged standalone owners and separately killable production stdio bridges.
Job Objects replace Unix PTY/session cleanup; a distinct owner and unrelated
process remain outside the killed owner's Job Object. No fixture cleanup runs
before product lifetime assertions.
"""
from __future__ import annotations
import argparse
import ctypes
from ctypes import wintypes as w
import json
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
import uuid
from windows_native_views import Fixture, sha256
from windows_ssh import Connection


def until(predicate, message, seconds=10):
    deadline = time.monotonic() + seconds
    while not predicate():
        assert time.monotonic() < deadline, message
        time.sleep(.02)


class Lifetimes(Fixture):
    def __init__(self, *args):
        super().__init__(*args)
        self.connections = []
        self.evidence = {}

    def owner(self, name, instance=None):
        instance = instance or name + '-' + uuid.uuid4().hex
        child = self.start([str(self.candidate), '-D', '-L', instance], name)
        def ready():
            assert child.poll() is None, f'owner exited before ready: {child.returncode}'
            return subprocess.run([str(self.candidate), '-L', instance, 'list-sessions'],
                                  capture_output=True, env=self.env, timeout=3).returncode == 0
        until(ready, 'standalone owner did not publish discovery')
        return child, instance

    def bridge(self, instance):
        child = subprocess.Popen([str(self.candidate), '--native-stdio'], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
                                 env=self.env, creationflags=subprocess.CREATE_NO_WINDOW)
        self.processes.append(child)
        connection = Connection(child.stdout, child.stdin, process=child)
        self.connections.append(connection)
        connection.handshake(instance)
        return connection

    def sleeping_pane(self, view, name):
        view.request('new-session', '-d', '-s', name, sys.executable, '-u', '-c',
                     'import os,time; print(os.getpid(),flush=True); time.sleep(180)')
        pid = int(view.request('display-message', '-p', '#{pane_pid}'))
        self.remember({'pane_processes': {name: pid}})
        return pid

    def missing(self, instance):
        result = subprocess.run([str(self.candidate), '-L', instance, 'list-sessions'],
                                capture_output=True, env=self.env, timeout=3)
        assert result.returncode != 0, 'killed owner still accepts commands'
        assert b'existing tmuxx instance unavailable' in result.stderr.lower(), result.stderr

    def foreground(self):
        owner, instance = self.owner('foreground')
        independent, other_instance = self.owner('foreground-independent')
        view, other = self.bridge(instance), self.bridge(other_instance)
        independent_pid = self.sleeping_pane(other, 'independent')
        marker = self.directory / 'foreground-pids.json'
        script = "import json,os,pathlib,subprocess,sys,time; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(180)']); marker=pathlib.Path(sys.argv[1]); pending=marker.with_name(marker.name+'.tmp'); pending.write_text(json.dumps([os.getpid(),child.pid])); pending.replace(marker); time.sleep(180)"
        view.request('new-session', '-d', '-s', 'owned', sys.executable, '-u', '-c', script, str(marker))
        until(marker.is_file, 'foreground descendant did not start')
        pids = json.loads(marker.read_text())
        self.remember({'pane_processes': dict(enumerate(pids))})
        assert all(self.alive(pid) for pid in pids)
        owner.kill()
        owner.wait(timeout=5)
        self.assert_dead(pids)
        self.missing(instance)
        assert self.alive(independent_pid) and independent.poll() is None
        assert int(other.request('display-message', '-p', '#{pane_pid}')) == independent_pid
        self.evidence['foreground'] = {'owned': pids, 'independent_owner': independent.pid,
                                       'independent_pane': independent_pid, 'owned_handles_signaled': True}
        other.request('kill-server')
        independent.wait(timeout=5)
        self.assert_dead([independent_pid])
        self.checks.append('abrupt_owner_death_closes_pty_and_foreground_descendants_without_harming_other_owner')

    def descendant_jobs(self):
        details = []
        for leader_exits in [False, True]:
            name = 'job-exited' if leader_exits else 'job-live'
            owner, instance = self.owner(name)
            independent, other_instance = self.owner(name + '-independent')
            unrelated = self.start([sys.executable, '-c', 'import time; time.sleep(180)'], name + '-unrelated')
            view, other = self.bridge(instance), self.bridge(other_instance)
            independent_pid = self.sleeping_pane(other, 'independent')
            marker, trigger = self.directory / (name + '.json'), self.directory / (name + '.exit')
            child_code = 'import signal,time; signal.signal(signal.SIGINT,signal.SIG_IGN); signal.signal(signal.SIGBREAK,signal.SIG_IGN); time.sleep(180)'
            script = ("import json,os,pathlib,signal,subprocess,sys,time\n"
                      "signal.signal(signal.SIGINT,signal.SIG_IGN); signal.signal(signal.SIGBREAK,signal.SIG_IGN)\n"
                      "children=[subprocess.Popen([sys.executable,'-c',sys.argv[3]],creationflags=flags) for flags in [0,subprocess.CREATE_NEW_PROCESS_GROUP]]\n"
                      "marker=pathlib.Path(sys.argv[1]); pending=marker.with_name(marker.name+'.tmp')\n"
                      "pending.write_text(json.dumps([os.getpid()]+[child.pid for child in children])); pending.replace(marker)\n"
                      "while not pathlib.Path(sys.argv[2]).exists(): time.sleep(.01)\nos._exit(0)")
            view.request('new-session', '-d', '-s', 'owned', sys.executable, '-u', '-c', script,
                         str(marker), str(trigger), child_code)
            until(marker.is_file, 'foreground/background descendants did not start')
            pids = json.loads(marker.read_text())
            self.remember({'pane_processes': dict(enumerate(pids))})
            assert all(self.alive(pid) for pid in pids)
            # Suspend the exact owner so normal EOF/reaper cleanup cannot mask
            # kill-on-close Job Object behavior after the pane leader exits.
            nt = ctypes.WinDLL('ntdll')
            nt.NtSuspendProcess.argtypes, nt.NtSuspendProcess.restype = [w.HANDLE], w.LONG
            assert nt.NtSuspendProcess(int(owner._handle)) == 0
            if leader_exits:
                trigger.write_text('exit')
                self.assert_dead([pids[0]])
            assert all(self.alive(pid) for pid in pids[1:])
            owner.kill()
            owner.wait(timeout=5)
            self.assert_dead(pids)
            self.missing(instance)
            assert self.alive(independent_pid) and independent.poll() is None and unrelated.poll() is None
            assert int(other.request('display-message', '-p', '#{pane_pid}')) == independent_pid
            details.append({'leader_exited_before_owner': leader_exits, 'owned': pids,
                            'unrelated': unrelated.pid, 'independent_owner': independent.pid,
                            'independent_pane': independent_pid, 'owned_handles_signaled': True})
            other.request('kill-server')
            independent.wait(timeout=5)
            self.assert_dead([independent_pid])
            unrelated.kill()
            unrelated.wait(timeout=5)
        self.evidence['job_object_death'] = details
        self.checks.append('killed_owner_watchdog_cleans_hup_ignoring_session_jobs_and_preserves_independent_sessions')

    def retained(self):
        owner, instance = self.owner('retained-bridge')
        view = self.bridge(instance)
        trigger, reply, script_path = (self.directory / name for name in ['query.trigger', 'query.reply', 'query.py'])
        script_path.write_text("""import ctypes,msvcrt,os,pathlib,sys,threading,time
k=ctypes.WinDLL('kernel32',use_last_error=True)
k.GetStdHandle.restype=ctypes.c_void_p
k.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]
assert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)
assert k.SetConsoleMode(k.GetStdHandle(-10),0x200)
assert k.SetConsoleMode(k.GetStdHandle(-11),0xD)
msvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)
data=bytearray()
def read():
 while True:
  part=os.read(0,1)
  if not part: return
  data.extend(part)
threading.Thread(target=read,daemon=True).start()
os.write(1,b'\\x1b[2J\\x1b[HREADY')
while not pathlib.Path(sys.argv[1]).exists(): time.sleep(.005)
os.write(1,b'\\r\\nDETACHED\\x1b[6n')
deadline=time.monotonic()+10
while not data.endswith(b'R'):
 assert time.monotonic()<deadline,repr(data)
 time.sleep(.005)
time.sleep(.25)
reply=pathlib.Path(sys.argv[2]); pending=reply.with_name(reply.name+'.tmp')
pending.write_bytes(data); pending.replace(reply)
time.sleep(180)
""", encoding='utf-8')
        view.request('new-session', '-d', '-s', 'original', sys.executable, '-c', 'import time; time.sleep(180)', subscribe=True)
        view.request('split-window', '-h', '-t', '%0', sys.executable, '-u', str(script_path), str(trigger), str(reply))
        view.request('tmuxx-retain-pane', '-t', '%0', 'off')
        view.request('tmuxx-retain-pane', '-t', '%1', 'on')
        view.request('new-session', '-d', '-s', 'linked', sys.executable, '-c', 'import time; time.sleep(180)')
        view.request('link-window', '-s', 'original:0', '-t', 'linked:5')
        pid = int(view.request('display-message', '-p', '-t', '%1', '#{pane_pid}'))
        pids = set(map(int, view.request('list-panes', '-a', '-F', '#{pane_pid}').splitlines()))
        self.remember({'pane_processes': dict(enumerate(pids))})
        until(lambda: view.request('capture-pane', '-p', '-t', '%1').startswith(b'READY'), 'raw pane did not become ready')
        view.process.kill()
        view.process.wait(timeout=5)
        # No native client remains while the pane emits and answers the query.
        trigger.write_text('go')
        until(reply.is_file, 'detached terminal query was not serviced')
        assert reply.read_bytes() == b'\x1b[2;9R', reply.read_bytes()
        relaunched = self.bridge(instance)
        until(lambda: sorted(relaunched.request('list-panes', '-a', '-F', '#{pane_id}').splitlines()) == [b'%1', b'%1', b'%2'],
              'ephemeral pane or shared retained links incorrect after last bridge death')
        assert int(relaunched.request('display-message', '-p', '-t', 'linked:5', '#{pane_pid}')) == pid
        links = sorted(relaunched.request('list-windows', '-a', '-F', '#{session_name}:#{window_index}:#{window_id}').splitlines())
        assert links == [b'linked:0:@1', b'linked:5:@0', b'original:0:@0'], links
        _, snapshot = relaunched.request('attach-session', '-t', 'linked', subscribe=True)
        assert any(pane['id'] == 1 for pane in snapshot['panes'])
        assert b'DETACHED' in relaunched.request('capture-pane', '-p', '-t', '%1')
        assert int(relaunched.request('display-message', '-p', '-t', '%1', '#{pane_pid}')) == pid
        assert self.alive(pid)
        self.evidence['retained'] = {'killed_bridge': view.process.pid, 'new_bridge': relaunched.process.pid,
                                     'same_pane_pid': pid, 'links': [line.decode() for line in links],
                                     'detached_query_reply_hex': reply.read_bytes().hex(),
                                     'incarnation': relaunched.hello['instance_id']}
        relaunched.request('kill-server')
        owner.wait(timeout=5)
        self.assert_dead(pids)
        self.checks.append('killed_client_relaunch_retains_selected_linked_pane_and_detached_queries_run_once')

    def incarnation(self):
        owner, instance = self.owner('reused-endpoint')
        before = self.bridge(instance)
        pid_before = self.sleeping_pane(before, 'before')
        _, state_before = before.request('attach-session', '-t', 'before', subscribe=True)
        owner.kill()
        owner.wait(timeout=5)
        self.assert_dead([pid_before])
        replacement, _ = self.owner('replacement-owner', instance)
        after = self.bridge(instance)
        pid_after = self.sleeping_pane(after, 'after')
        _, state_after = after.request('attach-session', '-t', 'after', subscribe=True)
        assert before.hello['instance_id'] != after.hello['instance_id']
        assert state_before['panes'][0]['id'] == state_after['panes'][0]['id'] == 0
        assert pid_before != pid_after
        self.evidence['incarnation'] = {'before': before.hello['instance_id'], 'after': after.hello['instance_id'],
                                        'pid_before': pid_before, 'pid_after': pid_after, 'reused_pane_id': 0}
        after.request('kill-server')
        replacement.wait(timeout=5)
        self.assert_dead([pid_after])
        self.checks.append('reused_endpoint_and_pane_id_have_a_new_server_incarnation')

    def cleanup(self):
        for connection in self.connections:
            connection.close()
        return super().cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['gui', 'candidate', 'report']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'win32':
        parser.error('requires native Windows')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix='windows-lifetime-', dir=args.report.parent))
    fixture = Lifetimes(args.gui.resolve(strict=True), args.candidate.resolve(strict=True), directory)
    report = {'schema': 1, 'kind': 'windows-lifetime', 'artifact_directory': str(directory),
              'os': platform.platform(), 'architecture': platform.machine(), 'ownership': 'Windows kill-on-close Job Objects',
              'gui_sha256': sha256(fixture.gui), 'candidate_sha256': sha256(fixture.candidate),
              'tests': [], 'failures': [], 'cleanup_remaining': []}
    try:
        fixture.foreground()
        fixture.descendant_jobs()
        fixture.retained()
        fixture.incarnation()
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
