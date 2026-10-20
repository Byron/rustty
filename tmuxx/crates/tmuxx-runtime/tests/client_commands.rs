#[cfg(windows)]
#[path = "client_commands/windows.rs"]
mod windows;
use std::time::Duration;
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use tmuxx_protocol::{Decoder, Event, Notification};
#[cfg(unix)]
use tmuxx_unix_ipc::{Connection, Identity, Schema, identify, msg, pack_argv};
#[cfg(windows)]
use windows::{Control, Pair};

#[cfg(unix)]
struct Control {
    _connection: Connection,
    input: UnixStream,
    output: UnixStream,
    decoder: Decoder,
    notifications: Vec<Notification>,
    failed: bool,
}
#[cfg(unix)]
impl Control {
    fn connect(path: &Path, pid: i32, session: &str) -> Self {
        let mut connection = Connection::new(
            UnixStream::connect(path).unwrap(),
            Schema::Modern { version: 8 },
        )
        .unwrap();
        let (input, writer) = UnixStream::pair().unwrap();
        let (reader, output) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut identity = Identity::current(true).unwrap();
        identity.pid = pid;
        identity.features = (1 << 4) | (1 << 18);
        identity.term = b"tmuxx-identity-test".to_vec();
        identity.cwd = b"/tmp".to_vec();
        identity.environment = vec![b"USER=forged-user-must-not-be-used".to_vec()];
        identify(
            &mut connection,
            &identity,
            Some(input.as_fd()),
            Some(output.as_fd()),
        )
        .unwrap();
        connection
            .send(
                msg::COMMAND,
                &pack_argv(&[
                    b"attach-session".to_vec(),
                    b"-t".to_vec(),
                    session.as_bytes().to_vec(),
                ])
                .unwrap(),
                None,
            )
            .unwrap();
        let mut control = Self {
            _connection: connection,
            input: writer,
            output: reader,
            decoder: Decoder::default(),
            notifications: Vec::new(),
            failed: false,
        };
        assert_eq!(control.reply(), Vec::<u8>::new());
        control.run("display -p ready");
        control.notifications.clear();
        control
    }
    fn reply(&mut self) -> Vec<u8> {
        let mut result = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let n = self.output.read(&mut buffer).unwrap();
            assert_ne!(n, 0);
            let mut ended = false;
            self.decoder
                .feed_into(&buffer[..n], |event| match event {
                    Event::CommandOutput(line) => {
                        result.extend(line);
                        result.push(b'\n');
                    }
                    Event::End { failed, .. } => {
                        self.failed = failed;
                        ended = true;
                    }
                    Event::Notification(notice) => self.notifications.push(notice),
                    _ => {}
                })
                .unwrap();
            if ended {
                return result;
            }
        }
    }
    fn detached(&mut self, hangup: bool, session: &str) {
        self._connection
            .stream()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        loop {
            let message = self._connection.recv().unwrap().expect("detach message");
            if message.kind == msg::FLAGS {
                continue;
            }
            assert_eq!(
                message.kind,
                if hangup { msg::DETACHKILL } else { msg::DETACH }
            );
            assert_eq!(message.payload, format!("{session}\0").as_bytes());
            self._connection.send(msg::EXITING, &[], None).unwrap();
            assert_eq!(self._connection.recv().unwrap().unwrap().kind, msg::EXITED);
            return;
        }
    }
    fn run(&mut self, command: &str) -> Vec<u8> {
        self.input.write_all(command.as_bytes()).unwrap();
        self.input.write_all(b"\n").unwrap();
        self.reply()
    }
    fn records(&mut self, command: &str) -> Vec<Event> {
        self.records_until(command, "__tmuxx_control_barrier", true)
    }
    fn records_until(&mut self, command: &str, marker_text: &str, barrier: bool) -> Vec<Event> {
        self.input.write_all(command.as_bytes()).unwrap();
        self.input.write_all(b"\n").unwrap();
        if barrier {
            writeln!(self.input, "display -p {marker_text}").unwrap();
        }
        let mut records = Vec::new();
        let mut marker = false;
        let mut complete = false;
        let mut number = 0;
        let mut previous = None;
        while !complete {
            let mut buffer = [0; 4096];
            let n = self.output.read(&mut buffer).unwrap();
            assert_ne!(n, 0);
            self.decoder
                .feed_into(&buffer[..n], |mut event| {
                    match &mut event {
                        Event::Begin(guard) => {
                            assert!(previous.is_none_or(|old| guard.number > old));
                            previous = Some(guard.number);
                            number += 1;
                            guard.time = 0;
                            guard.number = number;
                        }
                        Event::End { guard, .. } => {
                            guard.time = 0;
                            guard.number = number;
                            if marker {
                                complete = true;
                            }
                        }
                        Event::CommandOutput(line) if line == marker_text.as_bytes() => {
                            marker = true
                        }
                        Event::Notification(Notification::Output { .. }) => return,
                        _ => {}
                    }
                    records.push(event);
                })
                .unwrap();
        }
        records
    }
}

#[cfg(unix)]
struct Pair {
    binary: std::ffi::OsString,
    c: PathBuf,
    rust: PathBuf,
    _host: tmuxx_runtime::ServerHandle,
}
#[cfg(unix)]
impl Pair {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let prefix = format!(
            "/tmp/tx-cl-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let c = PathBuf::from(format!("{prefix}.c"));
        let rust = PathBuf::from(format!("{prefix}.r"));
        let mut options = tmuxx_runtime::Options::new(PathBuf::from(format!("{prefix}.n")));
        options.tmux_endpoint = Some(rust.clone());
        let host = tmuxx_runtime::start(options).unwrap();
        let pair = Self {
            binary: std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"),
            c,
            rust,
            _host: host,
        };
        for name in ["b", "a", "c"] {
            pair.command(&["new-session", "-d", "-s", name, "-n", "one", "sleep 120"]);
        }
        pair
    }
    fn command(&self, args: &[&str]) -> Vec<u8> {
        let run = |socket: &Path| {
            Command::new(&self.binary)
                .args(["-f", "/dev/null", "-S"])
                .arg(socket)
                .args(args)
                .env_remove("TMUX")
                .env("SHELL", "/bin/sh")
                .output()
                .unwrap()
        };
        let c = run(&self.c);
        let rust = run(&self.rust);
        assert_eq!(
            rust.status.code(),
            c.status.code(),
            "{args:?}: C {c:?}, Rust {rust:?}"
        );
        assert_eq!(rust.stderr, c.stderr, "{args:?}");
        assert_eq!(rust.stdout, c.stdout, "{args:?}");
        c.stdout
    }
    fn clients(&self, pid: i32, session: &str) -> (Control, Control) {
        (
            Control::connect(&self.rust, pid, session),
            Control::connect(&self.c, pid, session),
        )
    }
}
#[cfg(unix)]
impl Drop for Pair {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .arg("-S")
            .arg(&self.c)
            .arg("kill-server")
            .output();
    }
}
fn equal(rust: &mut Control, c: &mut Control, command: &str) {
    assert_eq!(rust.run(command), c.run(command), "{command}");
    assert_eq!(rust.failed, c.failed, "status {command}");
}
fn state(rust: &mut Control, c: &mut Control) {
    equal(
        rust,
        c,
        "display -p '#{client_name}:#{client_session}:#{client_last_session}:#{pane_id}:#{window_zoomed_flag}:#{client_readonly}'",
    );
    equal(
        rust,
        c,
        "list-clients -F '#{client_name}:#{client_session}:#{client_last_session}:#{client_readonly}'",
    );
    equal(
        rust,
        c,
        "list-sessions -F '#{session_id}:#{session_name}:#{session_path}:#{session_attached}'",
    );
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn attach_switch_targets_navigation_and_zoom_match_c() {
    let pair = Pair::new();
    let (mut rust, mut c) = pair.clients(46001, "b");
    let (mut other_rust, mut other_c) = pair.clients(46002, "a");
    for command in [
        "switch-client -n",
        "switch-client -p",
        "switch-client -l",
        "switch-client -n -O index",
        "switch-client -p -O creation",
        "switch-client -n -O name",
        "switch-client -n -O order",
        "switch-client -p -O title",
        "switch-client -c client-46002: -t c",
        "switch-client -c client-46002 -n",
        "attach -t a -c '/tmp/#{session_name}'",
        "attach -t a",
        "switch-client -r -t c",
        "switch-client -r -l",
    ] {
        equal(&mut rust, &mut c, command);
        state(&mut rust, &mut c);
        state(&mut other_rust, &mut other_c);
    }
    pair.command(&["new-window", "-d", "-t", "a", "-n", "two", "sleep 120"]);
    pair.command(&["split-window", "-d", "-t", "a:two", "sleep 120"]);
    pair.command(&["resize-pane", "-Z", "-t", "a:two.0"]);
    for command in [
        "switch-client -Z -t a:two.1",
        "switch-client -t a:two.0",
        "attach -t a:one",
    ] {
        equal(&mut rust, &mut c, command);
        state(&mut rust, &mut c);
    }
    pair.command(&["switch-client", "-c", "client-46002:", "-t", "b"]);
    state(&mut other_rust, &mut other_c);
    pair.command(&["switch-client", "-c", "missing", "-t", "b"]);
    pair.command(&["switch-client", "-c", "client-46002", "-O", "invalid"]);
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn detach_targets_all_session_and_attach_displacement_match_c_ipc() {
    for (command, hangup, detached_session) in [
        ("detach-client -t client-46002:", false, "b"),
        ("detach-client -P -t client-46002", true, "b"),
        ("detach-client -s b", false, "b"),
        ("detach-client -a", false, "b"),
        ("attach-session -d -t b", false, "b"),
        ("attach-session -x -t b", true, "b"),
        ("new-session -AD -s b", false, "b"),
        ("new-session -AX -s b", true, "b"),
    ] {
        let pair = Pair::new();
        let (mut rust, mut c) = pair.clients(46001, "a");
        let (mut other_rust, mut other_c) = pair.clients(46002, "b");
        equal(&mut rust, &mut c, command);
        other_rust.detached(hangup, detached_session);
        other_c.detached(hangup, detached_session);
        state(&mut rust, &mut c);
        equal(&mut rust, &mut c, "display -p usable");
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn client_session_notifications_are_targeted_like_c() {
    let pair = Pair::new();
    let (mut rust, mut c) = pair.clients(46001, "a");
    let (mut other_rust, mut other_c) = pair.clients(46002, "b");
    equal(&mut rust, &mut c, "display -p barrier");
    rust.notifications.clear();
    c.notifications.clear();
    other_rust.notifications.clear();
    other_c.notifications.clear();
    for command in [
        "switch-client -t c",
        "switch-client -c client-46002 -t a",
        "attach-session -t b",
    ] {
        equal(&mut rust, &mut c, command);
        equal(&mut rust, &mut c, "display -p barrier");
        equal(&mut other_rust, &mut other_c, "display -p barrier");
        let relevant = |client: &mut Control| {
            std::mem::take(&mut client.notifications)
                .into_iter()
                .filter(|notice| !matches!(notice, Notification::Output { .. }))
                .collect::<Vec<_>>()
        };
        assert_eq!(relevant(&mut rust), relevant(&mut c), "initiator {command}");
        assert_eq!(
            relevant(&mut other_rust),
            relevant(&mut other_c),
            "other {command}"
        );
    }
    equal(&mut rust, &mut c, "detach-client -t client-46002");
    other_rust.detached(false, "a");
    other_c.detached(false, "a");
    drop(other_rust);
    drop(other_c);
    for client in [&mut rust, &mut c] {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !client
            .notifications
            .iter()
            .any(|notice| matches!(notice, Notification::ClientDetached { .. }))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "client-detached notification"
            );
            assert_eq!(client.run("display -p barrier"), b"barrier\n");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(rust.notifications, c.notifications);
    assert!(
        rust.notifications
            .iter()
            .any(|notice| matches!(notice, Notification::ClientDetached { .. }))
    );
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn window_and_config_notifications_follow_each_client_session_like_c() {
    let pair = Pair::new();
    let (mut rust, mut c) = pair.clients(46001, "a");
    let (mut other_rust, mut other_c) = pair.clients(46002, "b");
    let relevant = |client: &mut Control| {
        std::mem::take(&mut client.notifications)
            .into_iter()
            .filter(|notice| !matches!(notice, Notification::Output { .. }))
            .collect::<Vec<_>>()
    };
    for (rust, c) in [(&mut rust, &mut c), (&mut other_rust, &mut other_c)] {
        equal(rust, c, "display -p barrier");
        relevant(rust);
        relevant(c);
    }
    for command in [
        "new-window -d -t a: -n two 'sleep 120'",
        "split-window -t a:two 'sleep 120'",
        "kill-pane -t a:two.1",
        "rename-window -t a:two renamed",
        "link-window -d -s a:renamed -t b:1",
        "select-window -t a:renamed",
        "split-window -d -t a:renamed 'sleep 120'",
        "resize-pane -Z -t a:renamed.0",
        "unlink-window -t a:renamed",
        "rename-window -t b:1 final",
        "kill-window -t b:1",
    ] {
        equal(&mut rust, &mut c, command);
        equal(&mut rust, &mut c, "display -p barrier");
        equal(&mut other_rust, &mut other_c, "display -p barrier");
        let expected = relevant(&mut c);
        if !command.starts_with("select-window") {
            assert!(!expected.is_empty(), "C {command}");
        }
        assert_eq!(relevant(&mut rust), expected, "initiator {command}");
        assert_eq!(
            relevant(&mut other_rust),
            relevant(&mut other_c),
            "other {command}"
        );
    }
    equal(
        &mut other_rust,
        &mut other_c,
        "refresh-client -f new-layouts",
    );
    pair.command(&[
        "new-window",
        "-d",
        "-t",
        "a:",
        "-n",
        "batch",
        "sleep 120",
        ";",
        "link-window",
        "-d",
        "-s",
        "a:batch",
        "-t",
        "b:1",
        ";",
        "split-window",
        "-d",
        "-t",
        "a:batch",
        "sleep 120",
        ";",
        "rename-window",
        "-t",
        "a:batch",
        "renamed",
        ";",
        "unlink-window",
        "-t",
        "a:renamed",
        ";",
        "rename-window",
        "-t",
        "b:1",
        "final",
        ";",
        "kill-window",
        "-t",
        "b:1",
    ]);
    equal(&mut rust, &mut c, "display -p barrier");
    equal(&mut other_rust, &mut other_c, "display -p barrier");
    assert_eq!(relevant(&mut rust), relevant(&mut c), "batched initiator");
    assert_eq!(
        relevant(&mut other_rust),
        relevant(&mut other_c),
        "batched other"
    );
    let path = pair.rust.with_extension("conf");
    std::fs::write(&path, b"not-a-command\nanother-unknown-command\n").unwrap();
    equal(
        &mut rust,
        &mut c,
        &format!("source-file '{}'", path.display()),
    );
    equal(&mut rust, &mut c, "display -p barrier");
    equal(&mut other_rust, &mut other_c, "display -p barrier");
    let expected = relevant(&mut c);
    assert!(
        matches!(expected.as_slice(), [Notification::ConfigError(_)]),
        "{expected:?}"
    );
    assert_eq!(relevant(&mut rust), expected);
    assert!(relevant(&mut other_c).is_empty());
    assert!(relevant(&mut other_rust).is_empty());
    std::fs::remove_file(path).unwrap();
    pair.command(&["set-window-option", "-t", "a:one", "allow-rename", "on"]);
    pair.command(&[
        "respawn-pane",
        "-k",
        "-t",
        "a:one.0",
        "printf '\\033kosc-name\\033\\\\'; sleep 120",
    ]);
    for client in [&mut rust, &mut c] {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while client.run("display -pt a:0 '#{window_name}'") != b"osc-name\n" {
            assert!(std::time::Instant::now() < deadline, "OSC rename deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    equal(&mut other_rust, &mut other_c, "display -p barrier");
    assert_eq!(
        relevant(&mut rust),
        relevant(&mut c),
        "application rename linked client"
    );
    assert_eq!(
        relevant(&mut other_rust),
        relevant(&mut other_c),
        "application rename unrelated client"
    );
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn automatic_resize_uses_linked_clients_and_window_caps_like_c() {
    let pair = Pair::new();
    let (mut rust, mut c) = pair.clients(46001, "a");
    let (mut other_rust, mut other_c) = pair.clients(46002, "b");
    pair.command(&["link-window", "-s", "a:0", "-t", "b:1"]);
    equal(&mut rust, &mut c, "refresh-client -C 100,30");
    equal(&mut other_rust, &mut other_c, "refresh-client -C 60,40");
    for command in [
        "resize-window -A",
        "resize-window -a",
        "resize-window -aA",
        "refresh-client -C @1:50x20",
        "resize-window -A",
        "resize-window -a",
        "refresh-client -C @1:",
        "resize-window -A",
        "refresh-client -f ignore-size",
        "resize-window -a",
        "resize-window -A",
    ] {
        equal(&mut rust, &mut c, command);
        equal(
            &mut rust,
            &mut c,
            "list-windows -aF '#{session_name}:#{window_id}:#{window_width}x#{window_height}:#{window-size}'",
        );
        equal(
            &mut rust,
            &mut c,
            "list-panes -aF '#{pane_id}:#{pane_width}x#{pane_height}'",
        );
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn grouped_sessions_preserve_complete_notification_order_like_c() {
    let pair = Pair::new();
    pair.command(&["new-window", "-d", "-t", "b:", "-n", "two", "sleep 120"]);
    let (mut rust, mut c) = pair.clients(46001, "a");
    let (mut other_rust, mut other_c) = pair.clients(46002, "b");
    let notices = |client: &mut Control| {
        std::mem::take(&mut client.notifications)
            .into_iter()
            .filter(|notice| !matches!(notice, Notification::Output { .. }))
            .collect::<Vec<_>>()
    };
    for (rust, c) in [(&mut rust, &mut c), (&mut other_rust, &mut other_c)] {
        equal(rust, c, "display -p barrier");
        notices(rust);
        notices(c);
    }
    for command in [
        "new-session -d -s grouped -t a",
        "new-session -d -s grouped2 -t grouped",
        "new-window -d -t a: -n two 'sleep 120'",
        "link-window -d -s b:one -t grouped:5",
        "link-window -d -s grouped:5 -t grouped:7",
        "select-window -t grouped:two",
        "unlink-window -t a:two",
        "swap-window -d -s a:one -t b:two",
        "kill-window -t a:5",
        "kill-session -t grouped",
        "kill-session -t grouped2",
        "new-session -d -s solo -n one 'sleep 120'",
        "new-session -d -s solocopy -t solo",
        "unlink-window -k -t solo:0",
        "new-session -d -s moved -n one 'sleep 120'",
        "move-window -d -s moved:0 -t a:9",
        "rename-session -t a renamed",
    ] {
        equal(&mut rust, &mut c, command);
        equal(&mut rust, &mut c, "display -p barrier");
        equal(&mut other_rust, &mut other_c, "display -p barrier");
        assert_eq!(notices(&mut rust), notices(&mut c), "initiator {command}");
        assert_eq!(
            notices(&mut other_rust),
            notices(&mut other_c),
            "other {command}"
        );
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn pane_moves_and_zoom_preserve_complete_notification_order_like_c() {
    let pair = Pair::new();
    pair.command(&["link-window", "-d", "-s", "a:one", "-t", "b:9"]);
    let (mut rust, mut c) = pair.clients(46001, "a");
    let (mut other_rust, mut other_c) = pair.clients(46002, "b");
    equal(
        &mut other_rust,
        &mut other_c,
        "refresh-client -f new-layouts",
    );
    let notices = |client: &mut Control| {
        std::mem::take(&mut client.notifications)
            .into_iter()
            .filter(|notice| !matches!(notice, Notification::Output { .. }))
            .collect::<Vec<_>>()
    };
    for (rust, c) in [(&mut rust, &mut c), (&mut other_rust, &mut other_c)] {
        equal(rust, c, "display -p barrier");
        notices(rust);
        notices(c);
    }
    for command in [
        "split-window -t a:one 'sleep 120'",
        "split-window -h -t a:one 'sleep 120'",
        "rotate-window -t a:one",
        "swap-pane -s a:one.0 -t a:one.2",
        "select-pane -t a:one.1",
        "resize-pane -Z -t a:one.1",
        "select-pane -t a:one.0",
        "resize-pane -Z -t a:one.0",
        "select-pane -Z -t a:one.2",
        "resize-pane -Z -t a:one.2",
        "break-pane -d -s a:one.2 -t b:1 -n broken",
        "join-pane -s b:broken.0 -t a:one.0",
        "kill-pane -t a:one.0",
        "split-window -t b:one 'sleep 120'",
        "join-pane -s b:one.1 -t a:one.0",
        "move-pane -s a:one.2 -t a:one.0",
        "swap-pane -s a:one.0 -t b:one.0",
        "resize-pane -Z -t a:one.0",
        "kill-pane -t a:one.1",
        "break-pane -d -s b:one.0 -t a:1 -n last-pane",
    ] {
        equal(&mut rust, &mut c, command);
        equal(&mut rust, &mut c, "display -p barrier");
        equal(&mut other_rust, &mut other_c, "display -p barrier");
        assert_eq!(notices(&mut rust), notices(&mut c), "initiator {command}");
        assert_eq!(
            notices(&mut other_rust),
            notices(&mut other_c),
            "other {command}"
        );
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn direct_control_preserves_prepared_blocks_environment_and_aliases_like_c() {
    let pair = Pair::new();
    let (mut rust, mut c) = pair.clients(46001, "a");
    for command in [
        "setenv -g CONTROL_PREP parsed",
        "if-shell -F 1 { setenv -g CONTROL_PREP changed; display -p \"block:$CONTROL_PREP\"; if-shell -F 1 'display -p \"string:$CONTROL_PREP\"' }",
        "set -s command-alias[90] 'late=display-message -p original'",
        "if-shell -F 1 { set -s command-alias[90] 'late=display-message -p changed'; if-shell -F 1 { late }; if-shell -F 1 'late' }",
        "set -g @value before",
        "run-shell -C -d 0.01 { set -g @value after; display -p 'block:#{@value}:#{command}' }",
        "run-shell -C -d 0.01 \"set -g @value second; display -p 'string:#{@value}:#{command}'\"",
        "set-hook -g after-rename-window { run-shell -C { set -g @value hook; set -gF @observed '#{@value}:#{command}' } }",
        "show-hooks -g after-rename-window",
        "rename-window -t a:one renamed",
        "show-options -gv @observed",
        "set-hook -gu after-rename-window",
        "set-buffer -b control payload",
        "delete-buffer -b control",
        "run-shell 'printf job; exit 3'",
        "if-shell 'true' { display -p async-if }",
        "if-shell -F 0 { no-such-command }",
        "if-shell -F 0 { display -p too many arguments }",
    ] {
        assert_eq!(rust.records(command), c.records(command), "{command}");
    }
    let path = pair.rust.with_extension("control.conf");
    for source in [
        "display -p first\nif-shell -F 1 { display -p nested }\ndisplay -p last\n",
        "not-a-command\n",
        "display -p first; source-file /nonexistent/tmuxx-control-source; display -p following\n",
    ] {
        std::fs::write(&path, source).unwrap();
        let command = format!("source-file '{}'", path.display());
        assert_eq!(rust.records(&command), c.records(&command), "{source}");
    }
    std::fs::remove_file(path).unwrap();
    let background =
        "run-shell -b -C -d 0.01 { set -g @background done; display -p background-done }";
    assert_eq!(
        rust.records_until(background, "background-done", false),
        c.records_until(background, "background-done", false)
    );
    assert_eq!(
        rust.records("show-options -gv @background"),
        c.records("show-options -gv @background")
    );
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn independent_session_window_closes_keep_explicit_order_and_causal_boundaries() {
    let pair = Pair::new();
    for name in ["two", "three", "four", "five"] {
        pair.command(&["new-window", "-d", "-t", "c:", "-n", name, "sleep 120"]);
    }
    let (mut rust, mut c) = pair.clients(46001, "a");
    equal(&mut rust, &mut c, "display -p barrier");
    rust.notifications.clear();
    c.notifications.clear();
    pair.command(&[
        "kill-session",
        "-t",
        "c",
        ";",
        "rename-window",
        "-t",
        "a:one",
        "survivor",
    ]);
    equal(&mut rust, &mut c, "display -p barrier");
    let notices = |client: &mut Control| {
        std::mem::take(&mut client.notifications)
            .into_iter()
            .filter(|notice| !matches!(notice, Notification::Output { .. }))
            .collect::<Vec<_>>()
    };
    let actual = notices(&mut rust);
    let reference = notices(&mut c);
    let sequence = |windows: &[u64]| {
        std::iter::once(Notification::SessionsChanged)
            .chain(windows.iter().map(|window_id| Notification::WindowClose {
                window_id: *window_id,
                linked: false,
            }))
            .chain(std::iter::once(Notification::WindowRenamed {
                window_id: 1,
                name: b"survivor".to_vec(),
                linked: true,
            }))
            .collect::<Vec<_>>()
    };
    // Only independent closes within this destroyed session differ. Neither
    // implementation may cross the session event or following command event.
    assert_eq!(
        reference,
        sequence(&[3, 4, 5, 6, 2]),
        "C RB-tree removal order"
    );
    assert_eq!(
        actual,
        sequence(&[2, 3, 4, 5, 6]),
        "Rust winlink removal order"
    );
    state(&mut rust, &mut c);
    equal(
        &mut rust,
        &mut c,
        "list-panes -aF '#{session_name}:#{window_id}:#{pane_id}:#{window_name}'",
    );
}
