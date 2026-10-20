#![cfg(unix)]
use std::{
    io::Write,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tmuxx_protocol::native::{Request, ServerMessage};
use tmuxx_runtime::{LocalClient, Options, ServerHandle};

struct Pair {
    host: ServerHandle,
    admin: Option<LocalClient>,
    binary: PathBuf,
    socket: PathBuf,
    controls: Vec<(LocalClient, Child)>,
}
fn request(client: &LocalClient, args: &[&str], control: bool) -> Vec<u8> {
    client
        .send(Request {
            request_id: 1,
            argv: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            stdin: None,
            subscribe: false,
            control,
        })
        .unwrap();
    reply(client, args)
}
fn reply(client: &LocalClient, args: &[&str]) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{args:?}");
            assert_eq!(reply.status.unwrap_or(0), 0, "{args:?}");
            return reply.output;
        }
        assert!(Instant::now() < deadline, "missing reply {args:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_child(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "C waiter did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}
impl Pair {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = format!(
            "/tmp/tx-life-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let host = tmuxx_runtime::start(Options::new(format!("{base}.r"))).unwrap();
        let pair = Self {
            admin: Some(host.connect().unwrap()),
            host,
            binary: std::env::var_os("TMUXX_C_TMUX")
                .expect("set TMUXX_C_TMUX")
                .into(),
            socket: format!("{base}.c").into(),
            controls: Vec::new(),
        };
        pair.run(&["new-session", "-d", "-s", "a", "-n", "fixed", "sleep 120"]);
        pair.run(&["set-option", "-s", "exit-empty", "off"]);
        pair.run(&["set-option", "-g", "default-shell", "/bin/sh"]);
        pair
    }
    fn oracle(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new(&self.binary)
            .args(["-f", "/dev/null", "-S"])
            .arg(&self.socket)
            .args(args)
            .env_remove("TMUX")
            .env("SHELL", "/bin/sh")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "C {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn run(&self, args: &[&str]) -> Vec<u8> {
        let c = self.oracle(args);
        let rust = request(self.admin.as_ref().unwrap(), args, false);
        assert_eq!(rust, c, "{args:?}");
        rust
    }
    fn attach(&mut self, session: &str, flags: &str) {
        let local = self.host.connect().unwrap();
        request(
            &local,
            &["attach-session", "-t", session, "-f", flags],
            true,
        );
        let child = Command::new(&self.binary)
            .arg("-S")
            .arg(&self.socket)
            .args([
                "-f",
                "/dev/null",
                "-C",
                "attach-session",
                "-t",
                session,
                "-f",
                flags,
            ])
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.controls.push((local, child));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let expected = format!("{}\n", self.controls.len());
            if self.oracle(&["display-message", "-p", "#{n;L:x}"]) == expected.as_bytes() {
                break;
            }
            assert!(Instant::now() < deadline, "C attach timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn counts(&self) -> Vec<u8> {
        self.run(&["list-sessions", "-F", "#{session_name}:#{session_attached}"])
    }
    fn detach_last(&mut self) {
        let (local, mut child) = self.controls.pop().unwrap();
        drop(local);
        let _ = child.kill();
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let expected = format!("{}\n", self.controls.len());
            let c = self.oracle(&["display-message", "-p", "#{n;L:x}"]);
            let r = request(
                self.admin.as_ref().unwrap(),
                &["display-message", "-p", "#{n;L:x}"],
                false,
            );
            if c == expected.as_bytes() && r == c {
                break;
            }
            assert!(Instant::now() < deadline, "detach timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Pair {
    fn drop(&mut self) {
        for (_, child) in &mut self.controls {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new(&self.binary)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}

#[test]
#[ignore = "requires current C tmux and socket/PTY access"]
fn destroyed_sessions_move_or_detach_attached_clients_by_policy() {
    for (policy, flags, counts) in [
        ("on", "", "a:0\nb:1\nd:0\n"),
        ("off", "", "a:0\nb:2\nd:0\n"),
        ("no-detached", "", "a:0\nb:1\nd:1\n"),
        ("previous", "", "a:0\nb:2\nd:0\n"),
        ("next", "", "a:0\nb:1\nd:1\n"),
        ("on", "no-detach-on-destroy", "a:0\nb:2\nd:0\n"),
    ] {
        let mut pair = Pair::new();
        for name in ["b", "c", "d"] {
            pair.run(&["new-session", "-d", "-s", name, "-n", "fixed", "sleep 120"]);
        }
        pair.attach("c", flags);
        std::thread::sleep(Duration::from_millis(20));
        pair.attach("b", "");
        pair.run(&["set-option", "-t", "c", "detach-on-destroy", policy]);
        pair.run(&[
            "set-hook",
            "-g",
            "client-session-changed",
            "set -gF @moved '#{hook_old_session_name}:#{hook_new_session_name}'",
        ]);
        assert_eq!(pair.counts(), b"a:0\nb:1\nc:1\nd:0\n");
        pair.run(&["kill-session", "-t", "c"]);
        assert_eq!(pair.counts(), counts.as_bytes(), "{policy} {flags}");
        pair.run(&["show-options", "-gqv", "@moved"]);
    }
}

#[test]
#[ignore = "requires current C tmux and socket/PTY access"]
fn destroy_unattached_respects_group_keep_policies() {
    for policy in ["on", "keep-last", "keep-group"] {
        let mut pair = Pair::new();
        pair.run(&["new-session", "-d", "-s", "b", "sleep 120"]);
        pair.run(&["new-session", "-d", "-s", "c", "-t", "b"]);
        pair.attach("b", "");
        pair.run(&["new-session", "-d", "-s", "d", "sleep 120"]);
        for name in ["b", "c", "d"] {
            pair.run(&["set-option", "-t", name, "destroy-unattached", policy]);
        }
        let expected = if policy == "keep-last" {
            b"a:0\nb:1\nd:0\n".as_slice()
        } else {
            b"a:0\nb:1\n"
        };
        assert_eq!(pair.counts(), expected, "{policy}");
        pair.detach_last();
        let expected = match policy {
            "on" => b"a:0\n".as_slice(),
            "keep-last" => b"a:0\nb:0\nd:0\n",
            _ => b"a:0\nb:0\n",
        };
        assert_eq!(pair.counts(), expected, "{policy}");
    }
}

#[test]
#[ignore = "requires current C tmux and socket/PTY access"]
fn self_destruction_wraps_session_order_and_respects_no_detach_fallback() {
    for (session, policy, flags, expected) in [
        ("a", "previous", "", "b:1\nc:2\n"),
        ("c", "next", "", "a:2\nb:1\n"),
        ("c", "no-detached", "", "a:1\nb:1\n"),
        ("c", "no-detached", "no-detach-on-destroy", "a:1\nb:2\n"),
        ("c", "on", "", "a:1\nb:1\n"),
        ("c", "off", "", "a:1\nb:2\n"),
    ] {
        let mut pair = Pair::new();
        for name in ["b", "c"] {
            pair.run(&["new-session", "-d", "-s", name, "sleep 120"]);
        }
        for name in [session, if session == "a" { "c" } else { "a" }, "b"] {
            pair.attach(name, if name == session { flags } else { "" });
            std::thread::sleep(Duration::from_millis(10));
        }
        pair.run(&["set-option", "-t", session, "detach-on-destroy", policy]);
        let (client, child) = &mut pair.controls[0];
        writeln!(child.stdin.as_mut().unwrap(), "kill-session -t {session}").unwrap();
        request(client, &["kill-session", "-t", session], true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while pair.oracle(&["list-sessions", "-F", "#{session_name}:#{session_attached}"])
            != expected.as_bytes()
        {
            assert!(
                Instant::now() < deadline,
                "C self-destruction timed out: {policy}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            pair.counts(),
            expected.as_bytes(),
            "{session} {policy} {flags}"
        );
    }
}

#[test]
#[ignore = "requires current C tmux and socket/PTY access"]
fn exit_unattached_flushes_channels_locks_and_event_waiters() {
    let mut pair = Pair::new();
    pair.attach("a", "");
    pair.run(&["wait-for", "-L", "locked"]);
    let mut waiters = Vec::new();
    for args in [
        vec!["wait-for", "ready"],
        vec!["wait-for", "-L", "locked"],
        vec!["wait-for", "-E", "window-renamed"],
    ] {
        let client = pair.host.connect().unwrap();
        let mut command = args.clone();
        command.extend([";", "display-message", "-p", "flushed"]);
        client
            .send(Request {
                request_id: 1,
                argv: command.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
                stdin: None,
                subscribe: false,
                control: false,
            })
            .unwrap();
        let child = Command::new(&pair.binary)
            .arg("-S")
            .arg(&pair.socket)
            .args(["-f", "/dev/null"])
            .args(&command)
            .env_remove("TMUX")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        waiters.push((client, child, args));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    for (_, _, args) in &waiters {
        let mut query = args.clone();
        query.insert(1, "-l");
        query.retain(|arg| *arg != "-L");
        loop {
            let c = pair.oracle(&query);
            let rust = request(pair.admin.as_ref().unwrap(), &query, false);
            if !c.is_empty() && !rust.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "waiter not registered {args:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    // exit-empty off overrides exit-unattached even after the final client detaches.
    pair.run(&["set-option", "-s", "exit-unattached", "on"]);
    pair.detach_last();
    pair.run(&["has-session", "-t", "a"]);
    for (client, child, _) in &mut waiters {
        assert!(client.try_recv().is_err());
        assert!(child.try_wait().unwrap().is_none());
    }
    pair.run(&["set-option", "-s", "exit-empty", "on"]);
    for (client, mut child, args) in waiters {
        assert_eq!(reply(&client, &args), b"flushed\n");
        wait_child(&mut child);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.stdout, b"flushed\n");
        assert!(output.stderr.is_empty());
    }
    // The idle native connection keeps the owner alive until it is closed.
    assert!(pair.host.endpoint.exists());
    drop(pair.admin.take());
    let endpoint = pair.host.endpoint.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    while endpoint.exists() {
        assert!(Instant::now() < deadline, "unattached owner did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires current C tmux and socket/PTY access"]
fn exit_unattached_waits_for_background_jobs_before_terminating() {
    let mut pair = Pair::new();
    let release = pair.socket.with_extension("release");
    let c_ready = pair.socket.with_extension("c-ready");
    let r_ready = pair.socket.with_extension("r-ready");
    let command = |ready: &PathBuf| {
        format!(
            ": > {}; while [ ! -e {} ]; do /bin/sleep 0.01; done",
            ready.display(),
            release.display()
        )
    };
    pair.oracle(&["run-shell", "-b", &command(&c_ready)]);
    request(
        pair.admin.as_ref().unwrap(),
        &["run-shell", "-b", &command(&r_ready)],
        false,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !c_ready.exists() || !r_ready.exists() {
        assert!(
            Instant::now() < deadline,
            "background jobs did not start: C {}, Rust {}, message {:?}",
            c_ready.exists(),
            r_ready.exists(),
            pair.admin.as_ref().unwrap().try_recv()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    pair.run(&["set-option", "-s", "exit-unattached", "on"]);
    pair.run(&["set-option", "-s", "exit-empty", "on"]);
    drop(pair.admin.take());
    std::thread::sleep(Duration::from_millis(60));
    // C marks -b jobs NOWAIT, sends SIGTERM and exits. Its stale socket inode
    // can remain after exit; existence was never evidence of a live C owner.
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(&pair.socket).is_ok() {
        assert!(Instant::now() < deadline, "C NOWAIT owner did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    // Native owners deliberately preserve and drain their retained jobs.
    assert!(std::os::unix::net::UnixStream::connect(&pair.host.endpoint).is_ok());
    std::fs::write(&release, b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(&pair.host.endpoint).is_ok() {
        assert!(
            Instant::now() < deadline,
            "native owner did not exit after jobs finished"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    for path in [&release, &c_ready, &r_ready] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
#[ignore = "requires current C tmux and socket/PTY access"]
fn activity_sorting_and_timestamp_relations_follow_attach_and_output() {
    let mut pair = Pair::new();
    for name in ["c", "b"] {
        pair.run(&["new-session", "-d", "-s", name, "sleep 120"]);
    }
    for format in [
        "#{S/t:#{session_name}|}",
        "#{S/tr:#{session_name}|}",
        "#{S/tn:#{session_name}|}",
        "#{S/ti:#{session_name}|}",
        "#{S:#{session_name}:#{>:#{session_created},0}:#{>=:#{session_activity},#{session_created}}:#{session_last_attached}|}",
    ] {
        pair.run(&["display-message", "-p", format]);
    }
    pair.attach("c", "");
    pair.attach("a", "");
    for format in [
        "#{S/t:#{session_name}|}",
        "#{S/tr:#{session_name}|}",
        "#{L/t:#{client_session}|}",
        "#{L/tr:#{client_session}|}",
        "#{L:#{>:#{client_created},0}:#{>=:#{client_activity},#{client_created}}|}",
        "#{S:#{session_name}:#{>=:#{session_created},#{start_time}}:#{>=:#{session_activity},#{session_created}}:#{>=:#{session_activity},#{session_last_attached}}|}",
    ] {
        pair.run(&["display-message", "-p", format]);
    }
    for name in ["z", "b"] {
        pair.run(&[
            "new-window",
            "-d",
            "-t",
            "a",
            "-n",
            name,
            "stty -echo; read line; printf READY; sleep 120",
        ]);
    }
    for format in [
        "#{W/t:#{window_name}|}",
        "#{W/tr:#{window_name}|}",
        "#{W/tn:#{window_name}|}",
        "#{W/ti:#{window_name}|}",
    ] {
        pair.run(&["display-message", "-p", "-t", "a", format]);
    }
    pair.run(&["send-keys", "-t", "a:z", "Enter"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let c = pair.oracle(&["capture-pane", "-p", "-t", "a:z"]);
        let rust = request(
            pair.admin.as_ref().unwrap(),
            &["capture-pane", "-p", "-t", "a:z"],
            false,
        );
        if c.starts_with(b"READY") && rust.starts_with(b"READY") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "application output did not arrive"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    pair.run(&[
        "display-message",
        "-p",
        "-t",
        "a",
        "#{W/t:#{window_name}:#{>:#{window_activity},0}|}",
    ]);
}

impl Pair {
    fn feed(&self, target: &str, data: &[u8]) {
        let args = ["display-message", "-I", "-t", target];
        let mut child = Command::new(&self.binary)
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(data).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let client = self.admin.as_ref().unwrap();
        client
            .send(Request {
                request_id: 1,
                argv: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
                stdin: Some(data.to_vec()),
                subscribe: false,
                control: false,
            })
            .unwrap();
        reply(client, &args);
    }
    fn wait_oracle_option(&self, name: &str, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.oracle(&["show-options", "-gqv", name]) != expected {
            assert!(
                Instant::now() < deadline,
                "C option {name} did not reach {:?}; got {:?}",
                String::from_utf8_lossy(expected),
                String::from_utf8_lossy(&self.oracle(&["show-options", "-gqv", name]))
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        while request(
            self.admin.as_ref().unwrap(),
            &["show-options", "-gqv", name],
            false,
        ) != expected
        {
            assert!(
                Instant::now() < deadline,
                "Rust option {name} did not reach {:?}",
                String::from_utf8_lossy(expected)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn alert_hooks_clear_on_selection_repeat_bells_and_reset_silence() {
    let mut p = Pair::new();
    p.run(&["new-window", "-d", "-t", "a:3", "-n", "alerts", ""]);
    p.attach("a", "");
    p.run(&["set-option", "-w", "-t", "a:3", "monitor-activity", "on"]);
    for (event, variable) in [
        ("alert-activity", "@activity"),
        ("alert-bell", "@bell"),
        ("alert-silence", "@silence"),
    ] {
        p.run(&["set-option", "-g", variable, ""]);
        p.run(&["set-hook", "-g", event, &format!("set-option -agF {variable} '#{{hook_session_name}}:#{{hook_window_index}}:#{{hook_window}}:#{{hook_pane}};'")]);
    }
    p.feed("%1", b"hello\x07\x07");
    p.wait_oracle_option("@activity", b"a:3:@1:;\n");
    p.wait_oracle_option("@bell", b"a:3:@1:;\n");
    p.feed("%1", b"again\x07");
    p.wait_oracle_option("@bell", b"a:3:@1:;a:3:@1:;\n");
    p.run(&["show-options", "-gqv", "@activity"]);
    p.run(&[
        "list-windows",
        "-F",
        "#{window_index}:#{window_flags}:#{window_activity_flag}:#{window_bell_flag}",
    ]);
    p.run(&["select-window", "-t", "a:3"]);
    p.run(&["list-windows", "-F", "#{window_index}:#{window_flags}"]);
    p.run(&["set-option", "activity-action", "none"]);
    p.feed("%1", b"current");
    p.run(&["show-options", "-gqv", "@activity"]);
    p.run(&["list-windows", "-F", "#{window_index}:#{window_flags}"]);
    p.run(&["select-window", "-t", "a:0"]);
    p.run(&["set-option", "-w", "-t", "a:3", "monitor-silence", "1"]);
    p.wait_oracle_option("@silence", b"a:3:@1:;\n");
    p.run(&[
        "display",
        "-p",
        "#{session_alert}:#{session_alerts}:#{session_silence_flag}",
    ]);
    p.run(&["select-window", "-t", "a:3"]);
    p.feed("%1", b"reset timer");
    p.run(&["select-window", "-t", "a:0"]);
    p.wait_oracle_option("@silence", b"a:3:@1:;a:3:@1:;\n");
    p.run(&["set-option", "-w", "-t", "a:3", "monitor-silence", "0"]);
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn shell_marker_hooks_have_c_pane_payloads() {
    let p = Pair::new();
    p.run(&["new-window", "-d", "-t", "a:3", "-n", "shell", ""]);
    p.run(&["set-option", "-g", "@shell", ""]);
    for event in [
        "pane-shell-prompt",
        "pane-command-started",
        "pane-command-finished",
    ] {
        p.run(&["set-hook", "-g", event, "set-option -agF @shell '#{hook}:#{hook_window}:#{hook_pane}:#{hook_session}:#{hook_window_index}:#{hook_command_line}:#{hook_command_status}:#{hook_command_duration};'"]);
    }
    p.feed(
        "%1",
        b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C;cmdline_url=echo%20ok\x07ok\x1b]133;D;7\x07",
    );
    p.wait_oracle_option("@shell", b"pane-shell-prompt:@1:%1:::::;pane-command-started:@1:%1:$0:3:echo ok::0;pane-command-finished:@1:%1:$0:3:echo ok:7:0;\n");
}
