#![cfg(windows)]
//! Preserve the original session/client/job lifecycle matrices on native Windows.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tmuxx_protocol::native::{Request, ServerMessage};
use tmuxx_runtime::{ClientContext, LocalClient, Options, ServerHandle};
#[path = "../../../conformance/remote_reference.rs"]
mod remote;
use remote::{RemoteControl, RemoteReference};

struct Pair {
    controls: Vec<(LocalClient, RemoteControl)>,
    admin: Option<LocalClient>,
    host: Option<ServerHandle>,
    reference: RemoteReference,
    dir: PathBuf,
}
fn native_args(args: &[&str]) -> Vec<Vec<u8>> {
    args.iter()
        .map(|value| match *value {
            "sleep 120" => b"python lifecycle_fixture.py idle".to_vec(),
            "stty -echo; read line; printf READY; sleep 120" => {
                b"python lifecycle_fixture.py interactive".to_vec()
            }
            "/bin/sh" => std::env::var("COMSPEC").unwrap().into_bytes(),
            value => value.as_bytes().to_vec(),
        })
        .collect()
}
fn request(client: &LocalClient, args: &[&str], control: bool) -> Vec<u8> {
    client
        .send(Request {
            request_id: 1,
            argv: native_args(args),
            stdin: None,
            subscribe: false,
            control,
        })
        .unwrap();
    reply(client, args)
}
fn reply(client: &LocalClient, args: &[&str]) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{args:?}");
            assert_eq!(reply.status.unwrap_or(0), 0, "{args:?}");
            return reply.output;
        }
        assert!(Instant::now() < deadline, "missing native reply {args:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn native_alive(endpoint: &std::path::Path) -> bool {
    tmuxx_runtime::windows::connect(endpoint).is_ok()
}
impl Pair {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "lifecycle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(format!("tmuxx-{name}"));
        std::fs::create_dir(&dir).unwrap();
        let source = include_bytes!("../../../conformance/lifecycle_fixture.py");
        std::fs::write(dir.join("lifecycle_fixture.py"), source).unwrap();
        let reference = RemoteReference::new();
        reference.put(b"lifecycle_fixture.py", source);
        let host = tmuxx_runtime::start(Options::new(
            tmuxx_runtime::windows::endpoint(&name).unwrap(),
        ))
        .unwrap();
        let admin = Some(
            host.connect_with_context(ClientContext {
                cwd: Some(dir.to_str().unwrap().to_owned()),
                ..ClientContext::default()
            })
            .unwrap(),
        );
        let pair = Self {
            controls: Vec::new(),
            admin,
            host: Some(host),
            reference,
            dir,
        };
        pair.run(&["new-session", "-d", "-s", "a", "-n", "fixed", "sleep 120"]);
        pair.run(&["set-option", "-s", "exit-empty", "off"]);
        pair.run(&["set-option", "-g", "default-shell", "/bin/sh"]);
        pair
    }
    fn oracle(&self, args: &[&str]) -> Vec<u8> {
        let fixture_args = args
            .iter()
            .map(|value| match *value {
                "python lifecycle_fixture.py job" => {
                    "exec /usr/bin/python3 lifecycle_fixture.py job"
                }
                "stty -echo; read line; printf READY; sleep 120" => {
                    "/usr/bin/python3 lifecycle_fixture.py interactive"
                }
                value => value,
            })
            .collect::<Vec<_>>();
        let output = self.reference.output(
            &fixture_args
                .iter()
                .map(|s| s.as_bytes())
                .collect::<Vec<_>>(),
            b"",
        );
        assert!(output.status.success(), "C {args:?}: {output:?}");
        output.stdout
    }
    fn run(&self, args: &[&str]) -> Vec<u8> {
        let c = self.oracle(args);
        let rust = request(self.admin.as_ref().unwrap(), args, false);
        assert_eq!(rust, c, "{args:?}");
        rust
    }
    fn attach(&mut self, session: &str, flags: &str) {
        let local = self.host.as_ref().unwrap().connect().unwrap();
        request(
            &local,
            &["attach-session", "-t", session, "-f", flags],
            true,
        );
        let child = self.reference.control_open_args(&[
            b"attach-session",
            b"-t",
            session.as_bytes(),
            b"-f",
            flags.as_bytes(),
        ]);
        self.controls.push((local, child));
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if self.oracle(&["display-message", "-p", "#{n;L:x}"])
                == format!("{}\n", self.controls.len()).as_bytes()
            {
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
        let (local, child) = self.controls.pop().unwrap();
        drop(local);
        child.close();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let expected = format!("{}\n", self.controls.len());
            let c = self.oracle(&["display-message", "-p", "#{n;L:x}"]);
            let rust = request(
                self.admin.as_ref().unwrap(),
                &["display-message", "-p", "#{n;L:x}"],
                false,
            );
            if c == expected.as_bytes() && rust == c {
                break;
            }
            assert!(Instant::now() < deadline, "detach timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Pair {
    fn drop(&mut self) {
        self.controls.clear();
        self.admin.take();
        self.host.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
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
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
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
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
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
        assert!(
            child
                .write(format!("kill-session -t {session}\n").as_bytes())
                .status
                .success()
        );
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
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
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
        let client = pair.host.as_ref().unwrap().connect().unwrap();
        let mut command = args.clone();
        command.extend([";", "display-message", "-p", "flushed"]);
        client
            .send(Request {
                request_id: 1,
                argv: native_args(&command),
                stdin: None,
                subscribe: false,
                control: false,
            })
            .unwrap();
        let child = pair
            .reference
            .command_open_args(&command.iter().map(|s| s.as_bytes()).collect::<Vec<_>>());
        waiters.push((client, child, args));
    }
    let deadline = Instant::now() + Duration::from_secs(8);
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
            assert!(Instant::now() < deadline, "waiter registration {args:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pair.run(&["set-option", "-s", "exit-unattached", "on"]);
    pair.detach_last();
    pair.run(&["has-session", "-t", "a"]);
    for (client, child, _) in &waiters {
        assert!(client.try_recv().is_err());
        assert!(child.exited().is_none());
    }
    pair.run(&["set-option", "-s", "exit-empty", "on"]);
    for (client, child, args) in waiters {
        assert_eq!(reply(&client, &args), b"flushed\n");
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut output = Vec::new();
        loop {
            let part = child.read();
            assert!(part.status.success());
            assert!(part.stderr.is_empty());
            output.extend(part.stdout);
            if let Some(status) = child.exited() {
                assert_eq!(status, 0);
                break;
            }
            assert!(Instant::now() < deadline, "C waiter deadline");
        }
        let part = child.read();
        assert!(part.status.success());
        assert!(part.stderr.is_empty());
        output.extend(part.stdout);
        assert_eq!(output, b"flushed\n");
    }
    let endpoint = pair.host.as_ref().unwrap().endpoint.clone();
    assert!(native_alive(&endpoint));
    pair.admin.take();
    let deadline = Instant::now() + Duration::from_secs(8);
    while native_alive(&endpoint) {
        assert!(
            Instant::now() < deadline,
            "idle native connection did not release owner"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
fn exit_unattached_waits_for_background_jobs_before_terminating() {
    let mut pair = Pair::new();
    let command = "python lifecycle_fixture.py job";
    pair.run(&["run-shell", "-b", command]);
    let deadline = Instant::now() + Duration::from_secs(8);
    while !pair.dir.join("ready").exists() || !pair.reference.exists(b"ready") {
        assert!(Instant::now() < deadline, "background jobs did not start");
        std::thread::sleep(Duration::from_millis(2));
    }
    let c_pid = String::from_utf8(pair.reference.get(b"job.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let c_job = pair.reference.watch_process(c_pid);
    assert!(pair.reference.process_alive(c_job));
    pair.run(&["set-option", "-s", "exit-unattached", "on"]);
    pair.run(&["set-option", "-s", "exit-empty", "on"]);
    pair.admin.take();
    std::thread::sleep(Duration::from_millis(60));
    let endpoint = pair.host.as_ref().unwrap().endpoint.clone();
    // C marks -b jobs NOWAIT and sends SIGTERM on automatic server exit.
    // The native owner deliberately drains retained job effects first.
    let deadline = Instant::now() + Duration::from_secs(8);
    while pair.reference.server_alive() || pair.reference.process_alive(c_job) {
        assert!(Instant::now() < deadline, "C NOWAIT owner/job did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(native_alive(&endpoint));
    std::fs::write(pair.dir.join("release"), b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while native_alive(&endpoint) {
        assert!(
            Instant::now() < deadline,
            "owners did not exit after background jobs finished"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
fn foreground_jobs_keep_live_owners_until_the_command_finishes() {
    let mut pair = Pair::new();
    let native = pair.host.as_ref().unwrap().connect().unwrap();
    let args = [
        "run-shell",
        "-c",
        pair.dir.to_str().unwrap(),
        "python lifecycle_fixture.py job",
        ";",
        "display-message",
        "-p",
        "drained",
    ];
    native
        .send(Request {
            request_id: 1,
            argv: native_args(&args),
            stdin: None,
            subscribe: false,
            control: false,
        })
        .unwrap();
    let remote = pair.reference.command_open_args(&[
        b"run-shell",
        b"exec /usr/bin/python3 lifecycle_fixture.py job",
        b";",
        b"display-message",
        b"-p",
        b"drained",
    ]);
    let deadline = Instant::now() + Duration::from_secs(8);
    while !pair.dir.join("ready").exists() || !pair.reference.exists(b"ready") {
        assert!(Instant::now() < deadline, "foreground jobs did not start");
        std::thread::sleep(Duration::from_millis(2));
    }
    pair.run(&["set-option", "-s", "exit-unattached", "on"]);
    pair.run(&["set-option", "-s", "exit-empty", "on"]);
    pair.admin.take();
    let endpoint = pair.host.as_ref().unwrap().endpoint.clone();
    assert!(native_alive(&endpoint));
    assert!(pair.reference.server_alive());
    assert!(native.try_recv().is_err());
    assert!(remote.exited().is_none());
    std::fs::write(pair.dir.join("release"), b"").unwrap();
    pair.reference.put(b"release", b"");
    assert_eq!(reply(&native, &["run-shell"]), b"drained\n");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut output = Vec::new();
    loop {
        let part = remote.read();
        assert!(part.status.success() && part.stderr.is_empty(), "{part:?}");
        output.extend(part.stdout);
        if let Some(code) = remote.exited() {
            assert_eq!(code, 0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "foreground C command did not complete"
        );
    }
    let part = remote.read();
    assert!(part.status.success() && part.stderr.is_empty(), "{part:?}");
    output.extend(part.stdout);
    assert_eq!(output, b"drained\n");
    drop(native);
    let deadline = Instant::now() + Duration::from_secs(8);
    while native_alive(&endpoint) || pair.reference.server_alive() {
        assert!(
            Instant::now() < deadline,
            "foreground owners did not finish after commands drained"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
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
    let pane = pair.run(&["display-message", "-p", "-t", "a:z", "#{pane_id}"]);
    let ready = format!(
        "interactive-{}.ready",
        std::str::from_utf8(&pane).unwrap().trim()
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while !pair.dir.join(&ready).exists() || !pair.reference.exists(ready.as_bytes()) {
        assert!(
            Instant::now() < deadline,
            "interactive child did not configure input"
        );
        std::thread::sleep(Duration::from_millis(2));
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
            "application output did not arrive: C {c:?}, Windows {rust:?}"
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
        let output = self
            .reference
            .output(&args.iter().map(|s| s.as_bytes()).collect::<Vec<_>>(), data);
        assert!(output.status.success(), "C feed: {output:?}");
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
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
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
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
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
