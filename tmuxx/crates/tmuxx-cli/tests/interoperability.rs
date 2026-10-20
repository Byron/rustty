#![cfg(unix)]

//! Explicit reference matrix: TMUXX_C_TMUX=/path/to/tmux cargo test -p tmuxx-cli --test interoperability -- --ignored
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tmuxx_protocol::{Decoder, Event};

#[cfg(target_os = "linux")]
#[path = "../../../test-support/socket_owner.rs"]
mod socket_owner;

struct Fixture {
    directory: PathBuf,
    c: PathBuf,
    c_socket: PathBuf,
    rust_socket: PathBuf,
    host: Option<tmuxx_runtime::ServerHandle>,
    #[cfg(target_os = "linux")]
    c_owner: Option<socket_owner::SocketOwner>,
    #[cfg(target_os = "linux")]
    rust_owner: socket_owner::SocketOwner,
}
impl Fixture {
    fn start() -> Self {
        let c = std::env::var_os("TMUXX_C_TMUX")
            .expect("set TMUXX_C_TMUX")
            .into();
        let directory = std::env::temp_dir().join(format!("tmuxx-matrix-{}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let c_socket = directory.join("c");
        let rust_socket = directory.join("rust");
        let mut options = tmuxx_runtime::Options::new(directory.join("native"));
        options.tmux_endpoint = Some(rust_socket.clone());
        let host = tmuxx_runtime::start(options).unwrap();
        #[cfg(target_os = "linux")]
        let rust_owner = socket_owner::SocketOwner::capture(
            &rust_socket,
            &std::env::current_exe().unwrap(),
            Some(std::process::id()),
        )
        .unwrap();
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut fixture = Self {
            directory,
            c,
            c_socket,
            rust_socket,
            host: Some(host),
            #[cfg(target_os = "linux")]
            c_owner: None,
            #[cfg(target_os = "linux")]
            rust_owner,
        };
        // This is the only matrix invocation allowed to create a C daemon.
        let created = launch(
            &fixture.c,
            &fixture.c_socket,
            &[
                "new-session",
                "-d",
                "-s",
                "test",
                "-x",
                "80",
                "-y",
                "24",
                "sleep 30",
            ],
            None,
            false,
        );
        assert!(created.status.success(), "{:?}", created);
        #[cfg(target_os = "linux")]
        {
            fixture.c_owner = Some(
                socket_owner::SocketOwner::capture(&fixture.c_socket, &fixture.c, None).unwrap(),
            );
        }
        let created = fixture.command(
            Path::new(env!("CARGO_BIN_EXE_tmuxx")),
            &fixture.rust_socket,
            &[
                "new-session",
                "-d",
                "-s",
                "test",
                "-x",
                "80",
                "-y",
                "24",
                "sleep 30",
            ],
            None,
        );
        assert!(created.status.success(), "{:?}", created);
        fixture
    }
    fn assert_owner(&self, socket: &Path) {
        #[cfg(target_os = "linux")]
        if socket == self.c_socket {
            self.c_owner
                .as_ref()
                .expect("C bootstrap must complete first")
                .check()
                .unwrap();
        } else if socket == self.rust_socket {
            self.rust_owner.check().unwrap();
        } else {
            panic!("unrecognized fixture endpoint {}", socket.display());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = socket;
    }
    fn command(&self, client: &Path, socket: &Path, args: &[&str], input: Option<&[u8]>) -> Output {
        self.assert_owner(socket);
        launch(client, socket, args, input, true)
    }
    fn control(&self, client: &Path, socket: &Path) -> ControlStream {
        self.assert_owner(socket);
        ControlStream::start(client, socket)
    }
}
fn launch(
    client: &Path,
    socket: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    no_start: bool,
) -> Output {
    let mut command = Command::new(client);
    if no_start {
        command.arg("-N");
    }
    let mut child = command
        .args(["-f", "/dev/null", "-S"])
        .arg(socket)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    wait(child)
}

impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        let cleanup_error = self
            .c_owner
            .as_ref()
            .and_then(|owner| owner.terminate().err());
        #[cfg(not(target_os = "linux"))]
        let _ = Command::new(&self.c)
            .args(["-N", "-S"])
            .arg(&self.c_socket)
            .arg("kill-server")
            .output();
        if let Some(host) = self.host.take() {
            let _ = host.shutdown();
        }
        #[cfg(target_os = "linux")]
        let preserve = cleanup_error.is_some()
            || self
                .c_owner
                .as_ref()
                .is_some_and(|owner| owner.endpoint_replaced().unwrap_or(true))
            || self.rust_owner.endpoint_replaced().unwrap_or(true);
        #[cfg(not(target_os = "linux"))]
        let preserve = false;
        if preserve || std::thread::panicking() {
            eprintln!("Preserved matrix fixture: {}", self.directory.display());
        } else {
            let _ = fs::remove_dir_all(&self.directory);
        }
        #[cfg(target_os = "linux")]
        if let Some(error) = cleanup_error {
            eprintln!("C fixture cleanup: {error}");
            assert!(
                std::thread::panicking(),
                "C fixture cleanup failed: {error}"
            );
        }
    }
}
fn wait(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("child timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn all_four_command_control_pairings() {
    let mut fixture = Fixture::start();
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            let run = |args: &[&str], input| fixture.command(client, socket, args, input);
            let result = run(&["display-message", "-p", "wire \\ bytes"], None);
            assert!(result.status.success(), "{client:?} {socket:?}: {result:?}");
            assert_eq!(result.stdout, b"wire \\ bytes\n");
            let result = run(&["select-pane", "-t", "%99999999"], None);
            assert!(
                !result.status.success(),
                "{client:?} {socket:?}: {result:?}"
            );
            assert!(!result.stderr.is_empty());
            let result = run(
                &[
                    "display-message",
                    "-p",
                    "BEFORE",
                    ";",
                    "select-pane",
                    "-t",
                    "%99999999",
                    ";",
                    "display-message",
                    "-p",
                    "AFTER",
                ],
                None,
            );
            assert!(
                !result.status.success(),
                "{client:?} {socket:?}: {result:?}"
            );
            assert_eq!(result.stdout, b"BEFORE\n");
            let result = run(&["load-buffer", "-"], Some(b"binary\0\xff\n"));
            assert!(result.status.success(), "{client:?} {socket:?}: {result:?}");
            let result = run(&["save-buffer", "-"], None);
            assert!(result.status.success(), "{client:?} {socket:?}: {result:?}");
            assert_eq!(result.stdout, b"binary\0\xff\n");

            let input_file = fixture.directory.join("input.buffer");
            let output_file = fixture.directory.join("output.buffer");
            let control_file = fixture.directory.join("control.buffer");
            fs::write(&input_file, b"file\0\xff\n").unwrap();
            assert!(
                run(&["load-buffer", input_file.to_str().unwrap()], None)
                    .status
                    .success()
            );
            assert!(
                run(&["save-buffer", output_file.to_str().unwrap()], None)
                    .status
                    .success()
            );
            assert!(
                run(&["save-buffer", "-a", output_file.to_str().unwrap()], None)
                    .status
                    .success()
            );
            assert_eq!(fs::read(&output_file).unwrap(), b"file\0\xff\nfile\0\xff\n");
            assert!(
                !run(
                    &[
                        "load-buffer",
                        fixture.directory.join("absent").to_str().unwrap()
                    ],
                    None
                )
                .status
                .success()
            );

            let mut child = Command::new(client)
                .arg("-N")
                .arg("-S")
                .arg(socket)
                .args(["-C", "attach-session", "-t", "test"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let commands = format!(
                "display-message -p BEFORE ; select-pane -t %99999999 ; display-message -p AFTER\nload-buffer {}\nsave-buffer {}\ndisplay-message -p RECOVERED\ndetach-client\n",
                input_file.display(),
                control_file.display()
            );
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(commands.as_bytes())
                .unwrap();
            let result = wait(child);
            assert!(result.status.success(), "{client:?} {socket:?}: {result:?}");
            let mut decoder = Decoder::default();
            let events = decoder.feed(&result.stdout).unwrap();
            decoder.finish().unwrap();
            let lines: Vec<_> = events
                .iter()
                .filter_map(|event| {
                    if let Event::CommandOutput(line) = event {
                        Some(line.as_slice())
                    } else {
                        None
                    }
                })
                .collect();
            assert!(
                lines.contains(&b"BEFORE".as_slice()),
                "{client:?} {socket:?}: {events:?}"
            );
            assert!(
                lines.contains(&b"RECOVERED".as_slice()),
                "{client:?} {socket:?}: {events:?}"
            );
            assert!(!lines.contains(&b"AFTER".as_slice()));
            let outcomes: Vec<_> = events
                .iter()
                .filter_map(|event| {
                    if let Event::End { failed, .. } = event {
                        Some(*failed)
                    } else {
                        None
                    }
                })
                .collect();
            let error = outcomes.iter().position(|failed| *failed).unwrap();
            assert!(outcomes[error + 1..].iter().any(|failed| !failed));
            assert!(run(&["has-session", "-t", "test"], None).status.success());
            assert_eq!(fs::read(&control_file).unwrap(), b"file\0\xff\n");
            double_control(client, socket);
            origin_context(&fixture, client, socket);
        }
    }
    // A missing Rust endpoint must fail closed even for commands that normally
    // start a C daemon. This catches accidental C/C conversion of the pairing.
    fixture.host.take().unwrap().shutdown().unwrap();
    for args in [
        vec!["new-session", "-d", "-s", "fallback", "sleep 30"],
        vec!["-C", "attach-session", "-t", "test"],
    ] {
        let failed = launch(&fixture.c, &fixture.rust_socket, &args, None, true);
        assert!(
            !failed.status.success(),
            "C client unexpectedly replaced missing Rust owner: {failed:?}"
        );
        assert!(
            !fixture.rust_socket.exists(),
            "failed C client created a replacement endpoint"
        );
    }
    #[cfg(target_os = "linux")]
    {
        // Cleanup is pinned to the original daemon, even after a new owner
        // deliberately binds exactly the same endpoint.
        let previous = fixture.c_owner.take().unwrap();
        fs::rename(&fixture.c_socket, fixture.directory.join("previous-c")).unwrap();
        let result = launch(
            &fixture.c,
            &fixture.c_socket,
            &["new-session", "-d", "-s", "replacement", "sleep 30"],
            None,
            false,
        );
        assert!(result.status.success(), "{result:?}");
        fixture.c_owner =
            Some(socket_owner::SocketOwner::capture(&fixture.c_socket, &fixture.c, None).unwrap());
        let replacement = fixture.c_owner.as_ref().unwrap();
        assert!(!previous.exited().unwrap());
        assert!(previous.check().is_err());
        assert!(previous.endpoint_replaced().unwrap());
        previous.terminate().unwrap();
        assert!(previous.exited().unwrap());
        previous.terminate().unwrap();
        replacement.check().unwrap();
        let result = launch(
            &fixture.c,
            &fixture.c_socket,
            &["display-message", "-p", "replacement-alive"],
            None,
            true,
        );
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, b"replacement-alive\n");
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn ordinary_terminal_clients_are_rejected_before_graph_mutation() {
    let fixture = Fixture::start();
    let graph = || {
        fixture
            .command(
                &fixture.c,
                &fixture.rust_socket,
                &[
                    "list-panes",
                    "-a",
                    "-F",
                    "#{session_name}|#{window_id}|#{pane_id}|#{pane_pid}",
                ],
                None,
            )
            .stdout
    };
    let before = graph();
    assert!(!before.is_empty());
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        for args in [
            vec![],
            vec!["new-session", "-s", "must-not-exist"],
            vec!["attach-session", "-t", "test"],
            vec!["new-session", "-A", "-s", "test"],
        ] {
            let result = fixture.command(client, &fixture.rust_socket, &args, None);
            assert!(!result.status.success(), "{client:?} {args:?}: {result:?}");
            assert!(result.stdout.is_empty());
            assert_eq!(
                result.stderr,
                b"ordinary terminal attachment is not supported; use Rustty or -C\n"
            );
            assert_eq!(graph(), before);
        }
    }
}

fn double_control(client: &Path, socket: &Path) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(client);
    command.arg("-N");
    command.arg("-S");
    command.arg(socket);
    command.args(["-CC", "attach-session", "-t", "test"]);
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        let mut chunk = [0; 4096];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if tx.send(chunk[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut bytes = Vec::new();
    let result = (|| -> Result<(), String> {
        while !bytes.windows(7).any(|w| w == b"\x1bP1000p") {
            bytes.extend(
                rx.recv_timeout(Duration::from_secs(5))
                    .map_err(|e| e.to_string())?,
            );
        }
        writer
            .write_all(b"display-message -p DOUBLE-CONTROL\ndetach-client\n")
            .map_err(|e| e.to_string())?;
        while !bytes.windows(2).any(|w| w == b"\x1b\\") {
            bytes.extend(
                rx.recv_timeout(Duration::from_secs(5))
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    drop(writer);
    drop(pair.master);
    drop(rx);
    task.join().unwrap();
    assert!(
        result.is_ok(),
        "{client:?} {socket:?}: {result:?}: {:?}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(status.success());
    let mut decoder = Decoder::dcs(1024 * 1024);
    let events = decoder.feed(&bytes).unwrap();
    decoder.finish().unwrap();
    assert!(events.contains(&Event::CommandOutput(b"DOUBLE-CONTROL".to_vec())));
}

fn origin_context(fixture: &Fixture, client: &Path, socket: &Path) {
    let result = Command::new(client)
        .arg("-N")
        .arg("-S")
        .arg(socket)
        .args([
            "new-session",
            "-d",
            "-s",
            "origin",
            "/bin/sh",
            "-c",
            "printf '%s\\n' \"$PWD\" \"$DISPLAY\"; sleep 30",
        ])
        .current_dir(&fixture.directory)
        .env("DISPLAY", "tmuxx-origin-marker")
        .output()
        .unwrap();
    assert!(result.status.success(), "{client:?} {socket:?}: {result:?}");
    let expected_path = fs::canonicalize(&fixture.directory).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = fixture.command(
            client,
            socket,
            &["capture-pane", "-p", "-t", "origin:0"],
            None,
        );
        assert!(result.status.success(), "{client:?} {socket:?}: {result:?}");
        let text = String::from_utf8_lossy(&result.stdout);
        if text.contains("tmuxx-origin-marker") {
            assert!(
                text.contains(expected_path.to_str().unwrap()),
                "{client:?} {socket:?}: {text}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{client:?} {socket:?}: origin context not propagated: {text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        fixture
            .command(client, socket, &["kill-session", "-t", "origin"], None)
            .status
            .success()
    );
}

struct ControlStream {
    child: Child,
    events: std::sync::mpsc::Receiver<Event>,
}
impl ControlStream {
    fn start(client: &Path, socket: &Path) -> Self {
        let mut command = Command::new(client);
        command.arg("-N").arg("-S").arg(socket);
        Self::spawn(command)
    }
    fn native(socket: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tmuxx"));
        command.arg("-N").arg("--native-endpoint").arg(socket);
        Self::spawn(command)
    }
    fn spawn(mut command: Command) -> Self {
        let mut child = command
            .args(["-C", "attach-session", "-t", "test"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = child.stdout.take().unwrap();
        let (tx, events) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut decoder = Decoder::default();
            let mut chunk = [0; 4096];
            while let Ok(n) = output.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                for event in decoder.feed(&chunk[..n]).expect("valid control bytes") {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
            }
        });
        let mut stream = Self { child, events };
        stream.result();
        stream
    }
    fn records(&mut self, command: &str) -> Vec<Event> {
        self.records_until(command, "__native_control_barrier", true)
    }
    fn records_until(&mut self, command: &str, marker_text: &str, barrier: bool) -> Vec<Event> {
        writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
        if barrier {
            writeln!(
                self.child.stdin.as_mut().unwrap(),
                "display -p {marker_text}"
            )
            .unwrap();
        }
        let mut records = Vec::new();
        let mut marker = false;
        let mut number = 0;
        let mut previous = None;
        loop {
            let mut event = self.next();
            let complete = matches!(&event, Event::End { .. }) && marker;
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
                }
                Event::CommandOutput(line) if line == marker_text.as_bytes() => marker = true,
                Event::Notification(tmuxx_protocol::Notification::Output { .. }) => continue,
                _ => {}
            }
            records.push(event);
            if complete {
                return records;
            }
        }
    }
    fn next(&self) -> Event {
        self.events
            .recv_timeout(Duration::from_secs(5))
            .expect("control event deadline")
    }
    fn result(&mut self) -> (Vec<Vec<u8>>, bool) {
        let mut lines = Vec::new();
        loop {
            match self.next() {
                Event::CommandOutput(line) => lines.push(line),
                Event::End { failed, .. } => return (lines, failed),
                _ => {}
            }
        }
    }
    fn command(&mut self, line: &str) -> (Vec<Vec<u8>>, bool) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{line}").unwrap();
        self.result()
    }
    fn ok(&mut self, line: &str) -> Vec<Vec<u8>> {
        let (lines, failed) = self.command(line);
        assert!(!failed, "{line}: {lines:?}");
        lines
    }
    fn notification(
        &self,
        select: impl Fn(&tmuxx_protocol::Notification) -> bool,
    ) -> tmuxx_protocol::Notification {
        loop {
            if let Event::Notification(notification) = self.next()
                && select(&notification)
            {
                return notification;
            }
        }
    }
}
impl Drop for ControlStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires HEAD C pane_last_output_time and real Unix sockets/PTYs"]
fn process_exit_signal_pipe_and_output_format_lifetimes_match_c() {
    process_lifetimes(true);
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn process_exit_signal_and_pipe_lifetimes_match_supported_c() {
    process_lifetimes(false);
}

fn process_lifetimes(output_time: bool) {
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let run = |args: &[&str]| {
            let result = fixture.command(&fixture.c, socket, args, None);
            assert!(result.status.success(), "{args:?}: {result:?}");
            String::from_utf8(result.stdout).unwrap()
        };
        let fields = || {
            run(&[
                "display-message",
                "-p",
                "-t",
                "%0",
                if output_time {
                    "#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{?pane_dead_time,1,0}|#{pane_pipe}|#{?pane_pipe_pid,1,0}|#{?pane_last_output_time,1,0}"
                } else {
                    "#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{?pane_dead_time,1,0}|#{pane_pipe}|#{?pane_pipe_pid,1,0}"
                },
            ])
        };
        let wait = |wanted: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let output = run(&["capture-pane", "-p", "-t", "%0"]);
                if output.contains(wanted) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "process marker missing: {output}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        run(&["set-option", "-w", "remain-on-exit", "on"]);
        let mut output = Vec::new();
        for ending in ["os._exit(137)", "os.kill(os.getpid(),signal.SIGKILL)"] {
            let script = format!(
                "import os,tty,signal\ntty.setraw(0)\nos.write(1,b'READY')\nos.read(0,1)\n{ending}"
            );
            run(&[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                "python3",
                "-u",
                "-c",
                &script,
            ]);
            wait("READY");
            output.push(fields());
            run(&["pipe-pane", "-t", "%0", "cat >/dev/null"]);
            output.push(fields());
            let pane_pid = run(&["display-message", "-p", "-t", "%0", "#{pane_pid}"]);
            let pipe_pid = run(&["display-message", "-p", "-t", "%0", "#{pane_pipe_pid}"]);
            assert!(pipe_pid.trim().parse::<u32>().unwrap() > 0 && pipe_pid != pane_pid);
            if !output_time {
                run(&["pipe-pane", "-t", "%0"]);
                output.push(fields());
            }
            run(&["send-keys", "-t", "%0", "-l", "q"]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fields().starts_with("1|") {
                assert!(Instant::now() < deadline, "process did not exit");
                std::thread::sleep(Duration::from_millis(5));
            }
            output.push(fields());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            for key in ["pane_dead_time", "pane_last_output_time"] {
                if key == "pane_last_output_time" && !output_time {
                    continue;
                }
                let time = run(&["display-message", "-p", "-t", "%0", &format!("#{{{key}}}")])
                    .trim()
                    .parse::<u64>()
                    .unwrap();
                assert!(time <= now && now - time < 10, "{key}: {time} versus {now}");
            }
        }
        observations.push(output);
    }
    assert_eq!(
        observations[1], observations[0],
        "process signal, timestamp, respawn and pipe lifetimes"
    );
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn control_color_reports_query_precedence_and_lifetime_match_c() {
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for (index, socket) in [&fixture.c_socket, &fixture.rust_socket]
        .into_iter()
        .enumerate()
    {
        let path = fixture.directory.join(format!("colors-{index}"));
        let script = format!(
            "import os,tty,select,time\ntty.setraw(0)\nos.write(1,b'\\x1b]10;#112233\\x07\\x1b]11;#040506\\x07READY')\nwhile True:\n os.read(0,1)\n os.write(1,b'\\x1b]10;?\\x07\\x1b]11;?\\x07\\x1b[?996n')\n data=b''\n deadline=time.monotonic()+0.1\n while True:\n  ready=select.select([0],[],[],max(0,deadline-time.monotonic()))[0]\n  if not ready: break\n  data+=os.read(0,4096)\n with open({:?},'ab') as f: f.write(data.hex().encode()+b'\\n')\n",
            path.to_str().unwrap()
        );
        let run = |args: &[&str]| {
            let output = fixture.command(&fixture.c, socket, args, None);
            assert!(output.status.success(), "{args:?}: {output:?}");
            output.stdout
        };
        let mut round = 0;
        let mut query = || {
            run(&["send-keys", "-t", "%0", "-l", "q"]);
            round += 1;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let lines = fs::read_to_string(&path).unwrap_or_default();
                if lines.lines().count() == round {
                    return lines.lines().last().unwrap().to_owned();
                }
                assert!(Instant::now() < deadline, "query response missing: {lines}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let spawn = || {
            run(&[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                "python3",
                "-u",
                "-c",
                &script,
            ]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !run(&["capture-pane", "-p", "-t", "%0"]).starts_with(b"READY") {
                assert!(Instant::now() < deadline, "query process not ready");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        spawn();
        let mut output = vec![query()];
        let mut first = fixture.control(&fixture.c, socket);
        let second = fixture.control(&fixture.c, socket);
        let report = |client: &mut ControlStream, channel: u8, value: &str| {
            let argument = tmuxx_command::display_argument(
                format!("%0:\x1b]{channel};{value}\x07").as_bytes(),
            );
            client.ok(&format!(
                "refresh-client -r {}",
                String::from_utf8(argument).unwrap()
            ));
        };
        for (channel, value) in [
            (10, "rgb:8000/ab00/ffff"),
            (11, "#ffffff"),
            (10, "#123"),
            (11, "cmyk:0/0/0/0.5"),
            (10, "1,-2,259junk"),
            (11, " gray50 "),
        ] {
            report(&mut first, channel, value);
            output.push(query());
        }
        run(&[
            "set-option",
            "-g",
            "window-active-style",
            "fg=#ffff00,bg=#ffffff",
        ]);
        output.push(query());
        drop(first);
        output.push(query());
        drop(second);
        output.push(query());
        let third = fixture.control(&fixture.c, socket);
        output.push(query());
        spawn();
        output.push(query());
        drop(third);
        observations.push(output);
    }
    assert_eq!(
        observations[1], observations[0],
        "query-only control colors, disconnect and respawn"
    );
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn window_creation_policy_aggressive_resize_and_limits_match_c() {
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let mut first = fixture.control(&fixture.c, socket);
        first.ok("refresh-client -C 100,40");
        let mut second = fixture.control(&fixture.c, socket);
        second.ok("refresh-client -C 60,20");
        let mut output = Vec::new();
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -C 120,50");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("select-window -t @0");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -C 110,45 ; display-message -p -t @0 '#{window_width}x#{window_height}'");
        output.push(first.result());
        first.ok("refresh-client -C 120,50");
        second.ok("refresh-client -C @0:45x12");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C @0:");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -f ignore-size");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -C 120,50");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -f ignore-size");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C 60,20");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -f '!ignore-size'");
        second.ok("refresh-client -f '!ignore-size'");
        first.ok("set-option -gw window-size smallest");
        first.ok("refresh-client -C 120,50");
        // Older C starts a short-lived shell for an empty new-window command;
        // new-session does so on both references. Keep sizing fixtures alive.
        output.push(
            first.command("new-window -d -P -F '#{window_width}x#{window_height}' 'sleep 60'"),
        );
        first.ok("new-session -d -s other 'sleep 60'");
        first.ok("link-window -d -s test:0 -t other:1");
        second.ok("switch-client -t other");
        assert_eq!(
            second.ok("display-message -p '#{window_index}'"),
            [b"0".to_vec()],
            "the other client's current window must remain independent"
        );
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        output.push(first.command("set-option -w -t @0 aggressive-resize on"));
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("select-window -t test:1");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("select-window -t other:1");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C @0:45x12");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        output.push(first.command("resize-window -t @0 -x 200 -y 70"));
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C @0:");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("set-option -w -t @1 window-size smallest");
        {
            let mut third = fixture.control(&fixture.c, socket);
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
            third.ok("refresh-client -C @0:");
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
            third.ok("refresh-client -C @0:30x10");
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
            third.ok("refresh-client -C @0:");
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
        }
        first.ok("set-option -gw window-size manual");
        first.ok("set-option -t test default-size 20000x1");
        output.push(
            first.command("new-window -d -t test: -P -F '#{window_width}x#{window_height}' ''"),
        );
        output.push(first.command(
            "new-session -d -s large -x 12000 -y 1 -P -F '#{window_width}x#{window_height}' 'sleep 60'",
        ));
        output.push(first.command("show-options -v -t large default-size"));
        observations.push(output);
    }
    let readable = observations
        .into_iter()
        .map(|output| {
            output
                .into_iter()
                .map(|(lines, failed)| {
                    (
                        lines
                            .into_iter()
                            .map(|line| String::from_utf8_lossy(&line).into_owned())
                            .collect::<Vec<_>>(),
                        failed,
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        readable[1], readable[0],
        "window size policy and creation dimensions"
    );
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn control_subscriptions_flags_sizes_and_flow_match_c() {
    use tmuxx_protocol::Notification;
    let fixture = Fixture::start();
    let legacy_read_only = fixture
        .command(
            &fixture.c,
            &fixture.c_socket,
            &["display-message", "-p", "#{version}"],
            None,
        )
        .stdout
        .starts_with(b"3.7");
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            let admin = |args: &[&str]| {
                let result = fixture.command(client, socket, args, None);
                assert!(
                    result.status.success(),
                    "{client:?} {socket:?}: {args:?}: {result:?}"
                );
                result.stdout
            };
            admin(&["set-option", "-t", "test", "@control-sub", "first"]);
            let mut control = fixture.control(client, socket);
            control.ok("refresh-client -B 'sub::#{@control-sub}' -B 'pane:%*:#{pane_id}'");
            let mut subscriptions = Vec::new();
            for _ in 0..2 {
                subscriptions.push(
                    control.notification(|n| matches!(n, Notification::SubscriptionChanged { .. })),
                );
            }
            assert_eq!(
                subscriptions,
                vec![
                    Notification::SubscriptionChanged {
                        fields: ["sub", "$0", "-", "-", "-"]
                            .map(|s| s.as_bytes().to_vec())
                            .to_vec(),
                        value: b"first".to_vec()
                    },
                    Notification::SubscriptionChanged {
                        fields: ["pane", "$0", "@0", "0", "%0"]
                            .map(|s| s.as_bytes().to_vec())
                            .to_vec(),
                        value: b"%0".to_vec()
                    },
                ],
                "{client:?} {socket:?}"
            );
            admin(&["set-option", "-t", "test", "@control-sub", "second"]);
            assert_eq!(
                control.notification(|n| matches!(n, Notification::SubscriptionChanged { .. })),
                Notification::SubscriptionChanged {
                    fields: ["sub", "$0", "-", "-", "-"]
                        .map(|s| s.as_bytes().to_vec())
                        .to_vec(),
                    value: b"second".to_vec()
                }
            );
            control.ok("refresh-client -B sub -B pane");
            control.ok("refresh-client -F no-output,ignore-size -f '!no-output,!ignore-size,pause-after=2'");
            assert_eq!(
                control.ok("display-message -p '#{client_flags}'"),
                [b"attached,focused,control-mode,pause-after=2,UTF-8".to_vec()]
            );
            control.ok("refresh-client -C 90x35");
            assert_eq!(
                control.ok("display-message -p '#{window_width}x#{window_height}'"),
                [b"90x35".to_vec()]
            );
            control.ok("refresh-client -C @0:60x20");
            assert_eq!(
                control.ok("display-message -p '#{window_width}x#{window_height}'"),
                [b"60x20".to_vec()]
            );
            control.ok("refresh-client -C @0:");
            assert_eq!(
                control.ok("display-message -p '#{window_width}x#{window_height}'"),
                [b"90x35".to_vec()]
            );
            assert_eq!(
                control.command("refresh-client -C 0,1"),
                (vec![b"size too small or too big".to_vec()], true)
            );
            let script = "import os,tty; tty.setraw(0); os.write(1,b'READY')\nwhile True: os.write(1,os.read(0,1))";
            admin(&[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                "python3",
                "-u",
                "-c",
                script,
            ]);
            let mut received = Vec::new();
            while !received.ends_with(b"READY") {
                if let Notification::Output { data, age_ms, .. } =
                    control.notification(|n| matches!(n, Notification::Output { .. }))
                {
                    assert!(age_ms.is_some());
                    received.extend(data);
                }
            }
            let paused = control.ok("refresh-client -A '%0:pause'");
            if paused.is_empty() {
                assert_eq!(
                    control.notification(|n| matches!(n, Notification::Pause { .. })),
                    Notification::Pause { pane_id: 0 }
                );
            } else {
                // tmux 3.7 prints its own flow notification inside the command guard.
                assert_eq!(paused, [b"%pause %0".to_vec()]);
            }
            admin(&["send-keys", "-t", "%0", "-l", "P"]);
            let deadline = Instant::now() + Duration::from_secs(3);
            while !admin(&["capture-pane", "-p", "-t", "%0"])
                .windows(6)
                .any(|part| part == b"READYP")
            {
                assert!(
                    Instant::now() < deadline,
                    "paused pane stopped processing input"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let continued = control.ok("refresh-client -A '%0:continue'");
            if continued.is_empty() {
                assert_eq!(
                    control.notification(|n| matches!(n, Notification::Continue { .. })),
                    Notification::Continue { pane_id: 0 }
                );
            } else {
                assert_eq!(continued, [b"%continue %0".to_vec()]);
            }
            admin(&["send-keys", "-t", "%0", "-l", "Q"]);
            match control.notification(|n| matches!(n, Notification::Output { .. })) {
                Notification::Output { data, age_ms, .. } => {
                    assert_eq!(data, b"Q");
                    assert!(age_ms.is_some());
                }
                _ => unreachable!(),
            }
            control.ok("refresh-client -f read-only");
            let denied = if socket == &fixture.c_socket && legacy_read_only {
                (Vec::new(), false)
            } else {
                (vec![b"client is read-only".to_vec()], true)
            };
            assert_eq!(control.command("set-option @forbidden yes"), denied);
            control.ok("detach-client");
            drop(control);
            let mut control = fixture.control(client, socket);
            control.ok("refresh-client -f wait-exit");
            control.ok("detach-client");
            control.notification(|n| matches!(n, Notification::Exit { .. }));
            assert!(
                control.child.try_wait().unwrap().is_none(),
                "wait-exit client exited before acknowledgement"
            );
            writeln!(control.child.stdin.as_mut().unwrap()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while control.child.try_wait().unwrap().is_none() {
                assert!(
                    Instant::now() < deadline,
                    "wait-exit did not accept empty acknowledgement"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn empty_respawn_and_pipe_lifecycle_match_c() {
    let fixture = Fixture::start();
    let legacy_pid = fixture
        .command(
            &fixture.c,
            &fixture.c_socket,
            &["display-message", "-p", "#{version}"],
            None,
        )
        .stdout
        .starts_with(b"3.7");
    for (pair, (client, socket)) in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))]
        .into_iter()
        .flat_map(|client| {
            [&fixture.c_socket, &fixture.rust_socket]
                .into_iter()
                .map(move |socket| (client, socket))
        })
        .enumerate()
    {
        let run = |args: &[&str], input| {
            let result = fixture.command(client, socket, args, input);
            assert!(
                result.status.success(),
                "{client:?} {socket:?}: {args:?}: {result:?}"
            );
            result.stdout
        };
        let capture = |pane: &str, expected: &[u8]| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let output = run(&["capture-pane", "-p", "-t", pane], None);
                if output.windows(expected.len()).any(|part| part == expected) {
                    return output;
                }
                assert!(
                    Instant::now() < deadline,
                    "{client:?} {socket:?}: missing {expected:?}: {output:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let empty = run(
            &[
                "split-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "%0",
                "",
            ],
            None,
        );
        let empty = std::str::from_utf8(&empty).unwrap().trim();
        assert_eq!(
            run(
                &[
                    "display-message",
                    "-p",
                    "-t",
                    empty,
                    "#{pane_pid}|#{pane_dead}|#{cursor_flag}"
                ],
                None
            ),
            if socket == &fixture.c_socket && legacy_pid {
                b"0|0|0\n".as_slice()
            } else {
                b"|0|0\n".as_slice()
            }
        );
        run(
            &["display-message", "-I", "-t", empty],
            Some(b"one\ntwo\n\x1b[31mred"),
        );
        let captured = run(
            &[
                "capture-pane",
                "-p",
                "-e",
                "-t",
                empty,
                "-S",
                "0",
                "-E",
                "2",
            ],
            None,
        );
        assert_eq!(captured, b"one\ntwo\n\x1b[31mred\x1b[39m\n");
        run(&["send-keys", "-t", empty, "-l", "ignored"], None);
        assert_eq!(
            run(
                &[
                    "capture-pane",
                    "-p",
                    "-e",
                    "-t",
                    empty,
                    "-S",
                    "0",
                    "-E",
                    "2"
                ],
                None
            ),
            captured
        );
        run(
            &[
                "respawn-pane",
                "-t",
                empty,
                "/bin/sh",
                "-c",
                "printf REVIVED; sleep 30",
            ],
            None,
        );
        capture(empty, b"REVIVED");
        let failed = fixture.command(client, socket, &["respawn-window", "-t", "test:0"], None);
        assert!(!failed.status.success());
        assert_eq!(
            failed.stderr,
            b"respawn window failed: window test:0 still active\n"
        );
        run(
            &[
                "respawn-window",
                "-k",
                "-t",
                "test:0",
                "-e",
                "TMUXX_PROCESS_MARK=REPLACED",
                "/bin/sh",
                "-c",
                "printf '%s' \"$TMUXX_PROCESS_MARK\"; sleep 30",
            ],
            None,
        );
        capture("%0", b"REPLACED");
        assert_eq!(
            run(
                &[
                    "list-panes",
                    "-t",
                    "test:0",
                    "-F",
                    "#{pane_id}|#{pane_active}|#{pane_width}x#{pane_height}"
                ],
                None
            ),
            b"%0|1|80x24\n"
        );
        let streamed = run(
            &[
                "split-window",
                "-d",
                "-I",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "%0",
            ],
            Some(b"stdin\nsecond"),
        );
        let streamed = std::str::from_utf8(&streamed).unwrap().trim();
        capture(streamed, b"stdin\nsecond");
        run(&["kill-pane", "-t", streamed], None);
        let script = "import os,tty; tty.setraw(0); os.write(1,b'READY')\nwhile True: os.write(1,os.read(0,1))";
        run(
            &[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                "python3",
                "-u",
                "-c",
                script,
            ],
            None,
        );
        capture("%0", b"READY");
        let log = fixture.directory.join(format!("pipe-{pair}"));
        let pipe = format!("cat > '{}'", log.display());
        run(&["pipe-pane", "-t", "%0", &pipe], None);
        assert_eq!(
            run(&["display-message", "-p", "-t", "%0", "#{pane_pipe}"], None),
            b"1\n"
        );
        run(&["send-keys", "-t", "%0", "-l", "A"], None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read(&log).unwrap_or_default() != b"A" {
            assert!(
                Instant::now() < deadline,
                "pipe did not capture raw output once"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        run(
            &[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                "python3",
                "-u",
                "-c",
                script,
            ],
            None,
        );
        capture("%0", b"READY");
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read(&log).unwrap_or_default() != b"AREADY" {
            assert!(
                Instant::now() < deadline,
                "pipe did not survive respawn without duplicated output: {:?}",
                fs::read(&log)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        run(&["pipe-pane", "-o", "-t", "%0", &pipe], None);
        assert_eq!(
            run(&["display-message", "-p", "-t", "%0", "#{pane_pipe}"], None),
            b"0\n"
        );
        run(&["send-keys", "-t", "%0", "-l", "B"], None);
        capture("%0", b"READYB");
        assert_eq!(fs::read(&log).unwrap(), b"AREADY");
        run(&["pipe-pane", "-I", "-t", "%0", "printf Q"], None);
        capture("%0", b"READYBQ");
        let deadline = Instant::now() + Duration::from_secs(5);
        while run(&["display-message", "-p", "-t", "%0", "#{pane_pipe}"], None) != b"0\n" {
            assert!(Instant::now() < deadline, "pipe child was not reaped");
            std::thread::sleep(Duration::from_millis(5));
        }
        run(
            &[
                "pipe-pane",
                "-IO",
                "-t",
                "%0",
                "python3 -u -c 'import os; os.read(0,1); os.write(1,b\"R\"); os.read(0,1)'",
            ],
            None,
        );
        run(&["send-keys", "-t", "%0", "-l", "S"], None);
        capture("%0", b"READYBQSR");
        let burst = "import os,tty; tty.setraw(0); os.write(1,b'READY'); os.read(0,1); os.write(1,b'Z'*262144)";
        let producer = run(
            &[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "python3",
                "-u",
                "-c",
                burst,
            ],
            None,
        );
        let producer = std::str::from_utf8(&producer).unwrap().trim();
        capture(producer, b"READY");
        let drained = fixture.directory.join(format!("drained-{pair}"));
        let consumer = format!(
            "python3 -u -c 'import os,time; f=open(\"{}\",\"wb\"); data=os.read(0,4096)\nwhile data: f.write(data); f.flush(); time.sleep(.001); data=os.read(0,4096)'",
            drained.display()
        );
        run(&["pipe-pane", "-t", producer, &consumer], None);
        run(&["send-keys", "-t", producer, "-l", "X"], None);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let bytes = fs::read(&drained).unwrap_or_default();
            if bytes.len() == 262144 {
                assert!(bytes.iter().all(|byte| *byte == b'Z'));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pipe lost trailing output at process exit: {} bytes",
                bytes.len()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn live_stdin_streams_before_eof_and_cancels_with_c() {
    let fixture = Fixture::start();
    let new_pane = fixture
        .command(
            &fixture.c,
            &fixture.c_socket,
            &["list-commands", "-F", "#{command_list_name}"],
            None,
        )
        .stdout
        .windows(9)
        .any(|name| name == b"new-pane\n");
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            let run = |args: &[&str]| {
                let output = fixture.command(client, socket, args, None);
                assert!(
                    output.status.success(),
                    "{client:?} {socket:?} {args:?}: {output:?}"
                );
                output.stdout
            };
            let base = run(&["new-window", "-dP", "-F", "#{pane_id}", "sleep 120"]);
            let base = std::str::from_utf8(&base).unwrap().trim();
            for name in if new_pane {
                vec!["split-window", "new-pane"]
            } else {
                vec!["split-window"]
            } {
                run(&["set-option", "-g", "@stdin-hook", "pending"]);
                run(&[
                    "set-hook",
                    "-g",
                    "after-split-window",
                    "set-option -g @stdin-hook done",
                ]);
                let mut child = Command::new(client)
                    .arg("-N")
                    .args(["-f", "/dev/null", "-S"])
                    .arg(socket)
                    .args([
                        name,
                        "-dIP",
                        "-F",
                        "#{pane_id}",
                        "-t",
                        base,
                        ";",
                        "display-message",
                        "-p",
                        "AFTER",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let mut input = child.stdin.take().unwrap();
                input.write_all(b"BEFORE_EOF\n").unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                let pane = loop {
                    let panes = run(&["list-panes", "-t", base, "-F", "#{pane_id}"]);
                    if let Some(pane) = std::str::from_utf8(&panes)
                        .unwrap()
                        .lines()
                        .find(|pane| *pane != base)
                    {
                        break pane.to_owned();
                    }
                    assert!(
                        Instant::now() < deadline,
                        "stdin pane was not created before EOF"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                loop {
                    if run(&["capture-pane", "-p", "-t", &pane]).starts_with(b"BEFORE_EOF") {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "stdin was not rendered before EOF"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert_eq!(run(&["show-options", "-gv", "@stdin-hook"]), b"pending\n");
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "stdin command exited before EOF"
                );
                // Cross the finite Request limit without accumulating bytes in the server.
                if name == "split-window" {
                    let (done, received) = std::sync::mpsc::sync_channel(1);
                    std::thread::spawn(move || {
                        let zeros = [0u8; 65536];
                        for _ in 0..257 {
                            input.write_all(&zeros).unwrap();
                        }
                        input.write_all(b"OVER_16_MIB\n").unwrap();
                        done.send(input).unwrap();
                    });
                    input = received
                        .recv_timeout(Duration::from_secs(30))
                        .expect("stdin stream stalled beyond the finite input limit");
                    let deadline = Instant::now() + Duration::from_secs(10);
                    loop {
                        if run(&["capture-pane", "-p", "-t", &pane])
                            .windows(11)
                            .any(|part| part == b"OVER_16_MIB")
                        {
                            break;
                        }
                        assert!(Instant::now() < deadline, "large stream was truncated");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                drop(input);
                let output = wait(child);
                assert!(output.status.success(), "{client:?} {socket:?}: {output:?}");
                assert_eq!(output.stdout, format!("{pane}\nAFTER\n").as_bytes());
                assert_eq!(run(&["show-options", "-gv", "@stdin-hook"]), b"done\n");
                run(&["set-hook", "-gu", "after-split-window"]);
                run(&["kill-pane", "-t", &pane]);
            }
            let display = run(&["split-window", "-dP", "-F", "#{pane_id}", "-t", base, ""]);
            let display = std::str::from_utf8(&display).unwrap().trim();
            let mut child = Command::new(client)
                .arg("-N")
                .args(["-f", "/dev/null", "-S"])
                .arg(socket)
                .args([
                    "display-message",
                    "-I",
                    "-t",
                    display,
                    ";",
                    "display-message",
                    "-p",
                    "AFTER_CANCEL",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut input = child.stdin.take().unwrap();
            input.write_all(b"DISPLAY_LIVE\n").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !run(&["capture-pane", "-p", "-t", display]).starts_with(b"DISPLAY_LIVE") {
                assert!(Instant::now() < deadline, "display -I waited for EOF");
                std::thread::sleep(Duration::from_millis(5));
            }
            run(&["kill-pane", "-t", display]);
            // C notices removed input panes on the next input callback.
            let _ = input.write_all(b"cancel");
            let output = wait(child);
            assert!(
                !output.status.success(),
                "removed pane left a successful input request: {output:?}"
            );
            assert!(
                !output
                    .stdout
                    .windows(12)
                    .any(|part| part == b"AFTER_CANCEL")
            );
            drop(input);
            run(&["kill-window", "-t", base]);
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn input_shutdown_and_pane_wait_follow_c() {
    for use_rust_client in [false, true] {
        for use_rust_server in [false, true] {
            let fixture = Fixture::start();
            let client = if use_rust_client {
                Path::new(env!("CARGO_BIN_EXE_tmuxx"))
            } else {
                &fixture.c
            };
            let socket = if use_rust_server {
                &fixture.rust_socket
            } else {
                &fixture.c_socket
            };
            let reference_has_wait = fixture
                .command(
                    &fixture.c,
                    &fixture.c_socket,
                    &[
                        "list-commands",
                        "-F",
                        "#{command_list_usage}",
                        "split-window",
                    ],
                    None,
                )
                .stdout
                .contains(&b'W');
            if reference_has_wait {
                let waited = fixture.command(
                    client,
                    socket,
                    &[
                        "split-window",
                        "-dWP",
                        "-F",
                        "WAITED",
                        "-t",
                        "%0",
                        "exit 7",
                        ";",
                        "display-message",
                        "-p",
                        "AFTER_WAIT",
                    ],
                    None,
                );
                assert_eq!(
                    waited.status.code(),
                    Some(7),
                    "{client:?} {socket:?}: {waited:?}"
                );
                assert_eq!(waited.stdout, b"WAITED\nAFTER_WAIT\n");
                let signalled = fixture.command(
                    client,
                    socket,
                    &[
                        "split-window",
                        "-dW",
                        "-t",
                        "%0",
                        "/bin/sh",
                        "-c",
                        "kill -TERM $$",
                    ],
                    None,
                );
                assert_eq!(
                    signalled.status.code(),
                    Some(143),
                    "{client:?} {socket:?}: {signalled:?}"
                );
                let mut child = Command::new(client)
                    .arg("-N")
                    .args(["-f", "/dev/null", "-S"])
                    .arg(socket)
                    .args([
                        "split-window",
                        "-dW",
                        "-t",
                        "%0",
                        "",
                        ";",
                        "display-message",
                        "-p",
                        "AFTER_REMOVAL",
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                let pane = loop {
                    let listed = fixture.command(
                        client,
                        socket,
                        &["list-panes", "-t", "%0", "-F", "#{pane_id}"],
                        None,
                    );
                    if let Some(pane) = std::str::from_utf8(&listed.stdout)
                        .unwrap()
                        .lines()
                        .find(|pane| *pane != "%0")
                    {
                        break pane.to_owned();
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                };
                assert!(child.try_wait().unwrap().is_none());
                assert!(
                    fixture
                        .command(client, socket, &["kill-pane", "-t", &pane], None)
                        .status
                        .success()
                );
                let removed = wait(child);
                assert_eq!(
                    removed.status.code(),
                    Some(129),
                    "{client:?} {socket:?}: {removed:?}"
                );
                assert_eq!(removed.stdout, b"AFTER_REMOVAL\n");
            }
            let empty = fixture.command(
                client,
                socket,
                &["split-window", "-dP", "-F", "#{pane_id}", "-t", "%0", ""],
                None,
            );
            assert!(empty.status.success());
            let pane = std::str::from_utf8(&empty.stdout).unwrap().trim();
            let mut child = Command::new(client)
                .arg("-N")
                .args(["-f", "/dev/null", "-S"])
                .arg(socket)
                .args(["display-message", "-I", "-t", pane])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut input = child.stdin.take().unwrap();
            input.write_all(b"SHUTDOWN_READY\n").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if fixture
                    .command(client, socket, &["capture-pane", "-p", "-t", pane], None)
                    .stdout
                    .starts_with(b"SHUTDOWN_READY")
                {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = fixture.command(client, socket, &["kill-server"], None);
            let stopped = wait(child);
            assert!(
                stopped.status.success(),
                "{client:?} {socket:?}: {stopped:?}"
            );
            drop(input);
        }
    }
}

#[test]
#[ignore = "requires real Unix sockets/PTYs"]
fn native_cli_streams_input_before_eof() {
    let endpoint =
        std::env::temp_dir().join(format!("tmuxx-native-stdin-{}.sock", std::process::id()));
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&endpoint)).unwrap();
    let client = Path::new(env!("CARGO_BIN_EXE_tmuxx"));
    let run = |args: &[&str]| {
        let output = Command::new(client)
            .arg("-N")
            .arg("--native-endpoint")
            .arg(&endpoint)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        output.stdout
    };
    run(&["new-session", "-d", "sleep 30"]);
    let pane = run(&["split-window", "-dP", "-F", "#{pane_id}", ""]);
    let pane = std::str::from_utf8(&pane).unwrap().trim();
    let mut child = Command::new(client)
        .arg("-N")
        .arg("--native-endpoint")
        .arg(&endpoint)
        .args([
            "display-message",
            "-I",
            "-t",
            pane,
            ";",
            "display-message",
            "-p",
            "AFTER",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"NATIVE_BEFORE_EOF\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !run(&["capture-pane", "-p", "-t", pane]).starts_with(b"NATIVE_BEFORE_EOF") {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(child.try_wait().unwrap().is_none());
    drop(input);
    let output = wait(child);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"AFTER\n");
    host.shutdown().unwrap();
}

#[test]
#[ignore = "requires C reference and real Unix sockets/PTYs"]
fn retained_key_panes_and_failed_key_policy_match_c() {
    let fixture = Fixture::start();
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            for name in ["split-window", "new-pane"] {
                let output = fixture.command(
                    client,
                    socket,
                    &[
                        name,
                        "-dkP",
                        "-F",
                        "#{pane_id}",
                        "-t",
                        "%0",
                        "/bin/sh",
                        "-c",
                        "printf retained; exit 7",
                    ],
                    None,
                );
                assert!(output.status.success(), "{client:?} {socket:?}: {output:?}");
                let pane = std::str::from_utf8(&output.stdout).unwrap().trim();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let state = fixture.command(
                        client,
                        socket,
                        &[
                            "display-message",
                            "-p",
                            "-t",
                            pane,
                            "#{pane_dead}|#{pane_dead_status}|#{cursor_flag}",
                        ],
                        None,
                    );
                    if state.stdout == b"1|7|0\n" {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "{client:?} {socket:?}: {state:?}"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                let retained = fixture.command(
                    client,
                    socket,
                    &["show-options", "-pv", "-t", pane, "remain-on-exit"],
                    None,
                );
                assert_eq!(retained.stdout, b"key\n");
                assert!(
                    fixture
                        .command(client, socket, &["send-keys", "-t", pane, "x"], None)
                        .status
                        .success()
                );
                assert_eq!(
                    fixture
                        .command(
                            client,
                            socket,
                            &["display-message", "-p", "-t", pane, "#{pane_dead}"],
                            None
                        )
                        .stdout,
                    b"1\n"
                );
                assert!(
                    fixture
                        .command(client, socket, &["kill-pane", "-t", pane], None)
                        .status
                        .success()
                );
            }
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn buffer_files_expand_in_queue_and_preserve_validation_and_io_errors() {
    let fixture = Fixture::start();
    let input = fixture.directory.join("formatted-input");
    let output = fixture.directory.join("formatted-output");
    fs::write(&input, b"formatted\0\xff\n").unwrap();
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        let mut observed = Vec::new();
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            let run = |args: &[&str]| fixture.command(client, socket, args, None);
            for args in [
                vec![
                    "set",
                    "-g",
                    "@file",
                    input.to_str().unwrap(),
                    ";",
                    "loadb",
                    "-b",
                    "file",
                    "#{@file}",
                    ";",
                    "saveb",
                    "-b",
                    "file",
                    "-",
                ],
                vec![
                    "set",
                    "-g",
                    "@file",
                    output.to_str().unwrap(),
                    ";",
                    "saveb",
                    "-b",
                    "file",
                    "#{@file}",
                ],
                vec!["saveb", "-a", "-b", "file", "#{@file}"],
                vec!["loadb", "-b", "file", "#{@file}"],
                vec!["saveb", "-b", "file", "#{l:-}"],
                vec!["loadb", "-b", "file", "#{l:/dev/null}"],
                vec!["saveb", "-b", "file", "-"],
                vec!["saveb", "-b", "missing", "/no/such/parent/file"],
                vec!["saveb", "-b", "file", "/no/such/parent/file"],
                vec!["loadb", "-b", "", "/no/such/parent/file"],
                vec!["loadb", "-b", "", input.to_str().unwrap()],
                vec!["loadb", "-z", "/no/such/parent/file"],
            ] {
                let result = run(&args);
                observed.push((
                    args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                    result.status.code(),
                    result.stdout,
                    result.stderr,
                ));
            }
            assert_eq!(
                fs::read(&output).unwrap(),
                b"formatted\0\xff\nformatted\0\xff\n"
            );
            fs::remove_file(&output).unwrap();
        }
        let (c, rust) = observed.split_at(observed.len() / 2);
        assert_eq!(rust, c, "client {client:?}");
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn cli_preserves_non_nul_argv_command_diagnostics_status_and_features() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::start();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let bytes: Vec<u8> = (1..=255).collect();
        for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
            let result = Command::new(client)
                .arg("-N")
                .args(["-N", "-S"])
                .arg(socket)
                .args(["display", "-pl"])
                .arg(std::ffi::OsStr::from_bytes(&bytes))
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert_eq!(result.stdout, [bytes.as_slice(), b"\n"].concat());
            assert!(result.stderr.is_empty());
        }
        for args in [
            vec!["display", "-p", ""],
            vec!["display", "-p", "-F", "x", "y"],
            vec!["list-c"],
            vec!["ls", "-z"],
            vec!["run-shell", "exit 7"],
            vec!["list-commands", "-h", "new"],
        ] {
            let c = fixture.command(&fixture.c, socket, &args, None);
            let rust = fixture.command(Path::new(env!("CARGO_BIN_EXE_tmuxx")), socket, &args, None);
            assert_eq!(
                (rust.status.code(), rust.stdout, rust.stderr),
                (c.status.code(), c.stdout, c.stderr),
                "{args:?}"
            );
        }
        for features in [
            vec!["-2", "-TRGB,clipboard", "-u", "-q"],
            vec!["-T256,rgb", "-T256@", "-2"],
            vec!["-2", "-T256@,focus", "-Trgb:unknown:mouse"],
        ] {
            let mut values = Vec::new();
            for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
                let mut child = Command::new(client)
                    .arg("-N")
                    .args(&features)
                    .args(["-C", "-S"])
                    .arg(socket)
                    .args(["attach-session", "-t", "test"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(b"display -p '#{client_termfeatures}|#{client_utf8}'\ndetach\n")
                    .unwrap();
                let output = wait(child);
                assert!(output.status.success(), "{output:?}");
                let mut decoder = Decoder::default();
                let replies: Vec<_> = decoder
                    .feed(&output.stdout)
                    .unwrap()
                    .into_iter()
                    .filter_map(|event| {
                        if let Event::CommandOutput(output) = event {
                            Some(output)
                        } else {
                            None
                        }
                    })
                    .collect();
                values.push(
                    replies
                        .into_iter()
                        .find(|value| value.contains(&b'|'))
                        .expect("feature response"),
                );
            }
            assert_eq!(values[0], values[1], "{features:?}");
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn display_all_formats_enumerates_present_callbacks_and_native_trace_is_bounded() {
    let fixture = Fixture::start();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        assert!(
            fixture
                .command(
                    &fixture.c,
                    socket,
                    &["rename-window", "-t", "test", "stable"],
                    None
                )
                .status
                .success()
        );
    }
    let maps: Vec<_> = [&fixture.c_socket, &fixture.rust_socket]
        .into_iter()
        .map(|socket| {
            let output = fixture.command(
                &fixture.c,
                socket,
                &["display-message", "-a", "-t", "test"],
                None,
            );
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| {
                    let (key, value) = line.split_once('=').unwrap();
                    (key.to_owned(), value.to_owned())
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .collect();
    assert_eq!(
        maps[0].keys().collect::<Vec<_>>(),
        maps[1].keys().collect::<Vec<_>>()
    );
    for (key, value) in &maps[0] {
        if ![
            "history_bytes",
            "history_all_bytes",
            "config_files",
            "version",
            "pid",
            "socket_path",
            "pane_pid",
            "pane_tty",
            "start_time",
            "session_created",
            "session_activity",
            "window_activity",
            "pane_last_output_time",
        ]
        .contains(&key.as_str())
        {
            assert_eq!(maps[1].get(key), Some(value), "{key}");
        }
    }
    let c = fixture.command(
        &fixture.c,
        &fixture.c_socket,
        &["display-message", "-pv", "#{session_name}"],
        None,
    );
    let rust = fixture.command(
        &fixture.c,
        &fixture.rust_socket,
        &["display-message", "-pv", "#{session_name}"],
        None,
    );
    assert!(c.status.success() && rust.status.success());
    assert!(c.stdout.ends_with(b"test\n"));
    assert_eq!(
        rust.stdout,
        b"tmuxx format input: \"#{session_name}\"\ntmuxx format result: test\ntest\n"
    );
    let long = "x".repeat(5000);
    let rust = fixture.command(
        &fixture.c,
        &fixture.rust_socket,
        &["display-message", "-plv", &long],
        None,
    );
    assert!(rust.status.success());
    assert_eq!(
        rust.stdout
            .windows(b"[truncated]".len())
            .filter(|window| *window == b"[truncated]")
            .count(),
        2
    );
    assert!(rust.stdout.ends_with(format!("{long}\n").as_bytes()));
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn buffer_formats_samples_filters_and_creation_times_match_c() {
    let fixture = Fixture::start();
    let samples = [
        b"\0".to_vec(),
        b"a\0b\x07\x08\x0b\x0c\x1b\x7f\\\xff\0".to_vec(),
        "é日本語\\\t\n".as_bytes().to_vec(),
        vec![b'x'; 199],
        vec![b'x'; 200],
        vec![b'x'; 201],
        vec![1; 70],
        [vec![b'x'; 199], "é".as_bytes().to_vec()].concat(),
    ];
    for sample in samples {
        let mut observations = Vec::new();
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            let output = fixture.command(&fixture.c, socket, &["load-buffer", "-"], Some(&sample));
            assert!(output.status.success(), "{output:?}");
            let mut results = Vec::new();
            for args in [
                vec![
                    "display-message",
                    "-p",
                    "#{buffer_name}|#{buffer_size}|#{buffer_sample}|#{buffer_full}",
                ],
                vec![
                    "list-buffers",
                    "-f",
                    "#{==:#{buffer_name},#{buffer_name}}",
                    "-F",
                    "#{buffer_name}=#{buffer_sample}",
                ],
                vec!["list-buffers", "-f", "0"],
            ] {
                let output = fixture.command(&fixture.c, socket, &args, None);
                assert!(output.status.success(), "{output:?}");
                results.push(output.stdout);
            }
            observations.push(results);
        }
        assert_eq!(observations[0], observations[1], "sample {sample:?}");
    }
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let run = |args: &[&str]| fixture.command(&fixture.c, socket, args, None);
        let query = |name: &str| {
            let result = run(&[
                "list-buffers",
                "-f",
                &format!("#{{==:#{{buffer_name}},{name}}}"),
                "-F",
                "#{buffer_created}",
            ]);
            assert!(result.status.success(), "{result:?}");
            String::from_utf8(result.stdout)
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap()
        };
        assert!(
            run(&["set-buffer", "-b", "created", "value"])
                .status
                .success()
        );
        let created = query("created");
        assert!(created > 0);
        assert!(
            run(&["set-buffer", "-b", "created", "-n", "renamed"])
                .status
                .success()
        );
        assert_eq!(query("renamed"), created);
        std::thread::sleep(Duration::from_millis(1100));
        assert!(
            run(&["set-buffer", "-ab", "renamed", "more"])
                .status
                .success()
        );
        assert!(query("renamed") > created);
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn display_message_uses_control_notification_and_print_flag_like_c() {
    let fixture = Fixture::start();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let ordinary = fixture.command(
            &fixture.c,
            socket,
            &["display-message", "no recipient"],
            None,
        );
        assert!(ordinary.status.success());
        assert!(ordinary.stdout.is_empty());
        assert!(ordinary.stderr.is_empty());
        let mut child = Command::new(&fixture.c)
            .arg("-N")
            .args(["-C", "-S"])
            .arg(socket)
            .args(["attach-session", "-t", "test"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                b"display-message 'message body'\ndisplay-message -p 'print body'\ndetach-client\n",
            )
            .unwrap();
        let output = wait(child);
        assert!(output.status.success(), "{output:?}");
        let events = Decoder::default().feed(&output.stdout).unwrap();
        let messages: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if let Event::Notification(tmuxx_protocol::Notification::Message(data)) = event {
                    Some(data.as_slice())
                } else {
                    None
                }
            })
            .collect();
        let printed: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if let Event::CommandOutput(data) = event {
                    Some(data.as_slice())
                } else {
                    None
                }
            })
            .collect();
        assert!(messages.is_empty());
        assert_eq!(
            printed,
            [b"%message message body".as_slice(), b"print body"],
            "{socket:?}"
        );
    }
}

#[test]
#[ignore = "requires an explicit current C reference and real Unix sockets/PTYs"]
fn paste_bytes_sanitizing_separator_brackets_and_deletion_match_c() {
    let fixture = Fixture::start();
    let script = fixture.directory.join("paste.py");
    fs::write(
        &script,
        r#"import os,sys,tty,time
tty.setraw(0)
os.write(1, (b'\x1b[?2004h' if sys.argv[2] == '1' else b'\x1b[?2004l') + b'\x1b[c')
reply=b''
while not reply.endswith(b'c'): reply+=os.read(0,1)
open(sys.argv[1]+'.ready','wb').close()
data=b''
marker=b'tmuxx-buffer-end-marker'
while not data.endswith(marker): data+=os.read(0,4096)
open(sys.argv[1], 'wb').write(data[:-len(marker)])
time.sleep(60)
"#,
    )
    .unwrap();
    let payload = [
        (0..=255).collect::<Vec<u8>>(),
        "é日本語\u{80}\n".as_bytes().to_vec(),
        b"\xc0\xaf\xed\xa0\x80\xf4\x90\x80\x80".to_vec(),
    ]
    .concat();
    let marker = "tmuxx-buffer-end-marker";
    for (case, (flags, bracket, disabled)) in [
        (vec![], false, false),
        (vec!["-S"], false, false),
        (vec!["-r"], false, false),
        (vec!["-r", "-s", "|"], false, false),
        (vec!["-s", ""], false, false),
        (vec!["-s", "\x1b[31m"], false, false),
        (vec!["-p"], false, false),
        (vec!["-p"], true, false),
        (vec!["-pSd"], true, false),
        (vec!["-pd"], true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let mut results = Vec::new();
        for (server, socket) in [&fixture.c_socket, &fixture.rust_socket]
            .into_iter()
            .enumerate()
        {
            let run = |args: &[&str], input| fixture.command(&fixture.c, socket, args, input);
            let result = fixture.directory.join(format!("paste-{case}-{server}"));
            let output = run(
                &[
                    "new-window",
                    "-d",
                    "-t",
                    "test:9",
                    "python3",
                    "-u",
                    script.to_str().unwrap(),
                    result.to_str().unwrap(),
                    if bracket { "1" } else { "0" },
                ],
                None,
            );
            assert!(output.status.success(), "{output:?}");
            let ready = result.with_extension("ready");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                assert!(Instant::now() < deadline, "paste child not ready");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                run(
                    &["load-buffer", "-b", "paste", "-"],
                    Some(payload.as_slice())
                )
                .status
                .success()
            );
            if disabled {
                assert!(
                    run(&["select-pane", "-d", "-t", "test:9.0"], None)
                        .status
                        .success()
                );
            }
            let args = [
                vec!["paste-buffer", "-b", "paste", "-t", "test:9.0"],
                flags.clone(),
            ]
            .concat();
            let output = run(&args, None);
            assert!(output.status.success(), "{args:?}: {output:?}");
            let saved = run(&["show-buffer", "-b", "paste"], None);
            if flags.iter().any(|flag| flag.contains('d')) {
                assert!(!saved.status.success());
                assert_eq!(saved.stderr, b"no buffer paste\n");
            } else {
                assert!(saved.status.success());
                assert_eq!(saved.stdout, payload);
            }
            if disabled {
                assert!(
                    run(&["select-pane", "-e", "-t", "test:9.0"], None)
                        .status
                        .success()
                );
            }
            assert!(
                run(&["send-keys", "-l", "-t", "test:9.0", marker], None)
                    .status
                    .success()
            );
            while !result.exists() {
                assert!(
                    Instant::now() < deadline,
                    "paste child didn't receive marker"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            // The child closes the result file before this independent command completes.
            assert!(run(&["kill-window", "-t", "test:9"], None).status.success());
            results.push(fs::read(result).unwrap());
        }
        assert_eq!(results[0], results[1], "case {case}: {flags:?}");
        if disabled {
            assert!(results[1].is_empty());
        }
        if flags == ["-S"] {
            assert_eq!(
                results[1],
                payload
                    .iter()
                    .map(|byte| if *byte == b'\n' { b'\r' } else { *byte })
                    .collect::<Vec<_>>()
            );
        }
        if bracket && !disabled {
            assert!(results[1].starts_with(b"\x1b[200~") && results[1].ends_with(b"\x1b[201~"));
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn same_user_acl_lists_owner_and_rejects_changes_without_mutation() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::start();
    let list = |socket: &Path| fixture.command(&fixture.c, socket, &["server-access", "-l"], None);
    let c = list(&fixture.c_socket);
    let rust = list(&fixture.rust_socket);
    assert!(c.status.success() && rust.status.success());
    assert_eq!(c.stdout, rust.stdout);
    let before = fixture
        .command(
            &fixture.c,
            &fixture.rust_socket,
            &[
                "list-panes",
                "-aF",
                "#{pane_id}|#{pane_pid}|#{pane_width}|#{pane_height}",
            ],
            None,
        )
        .stdout;
    for flags in [
        vec!["-a"],
        vec!["-r"],
        vec!["-w"],
        vec!["-d"],
        vec!["-ad"],
        vec!["-rw"],
        vec!["-ag"],
        vec![],
    ] {
        let mut args = vec!["server-access"];
        args.extend(flags);
        args.push("nobody");
        let output = fixture.command(&fixture.c, &fixture.rust_socket, &args, None);
        assert!(!output.status.success(), "{args:?}");
        assert_eq!(
            output.stderr,
            b"server-access changes are unsupported: server is same-user-only\n"
        );
        assert_eq!(list(&fixture.rust_socket).stdout, rust.stdout);
        for path in [&fixture.rust_socket, &fixture.directory.join("native")] {
            let metadata = fs::metadata(path).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        }
        assert_eq!(
            fixture
                .command(
                    &fixture.c,
                    &fixture.rust_socket,
                    &[
                        "list-panes",
                        "-aF",
                        "#{pane_id}|#{pane_pid}|#{pane_width}|#{pane_height}"
                    ],
                    None
                )
                .stdout,
            before
        );
    }
    // C's extra ACL entry remains confined behind the fixture's private directory.
    assert!(
        fixture
            .command(
                &fixture.c,
                &fixture.c_socket,
                &["server-access", "-a", "nobody"],
                None
            )
            .status
            .success()
    );
    assert!(
        list(&fixture.c_socket)
            .stdout
            .windows(b"nobody (U,W)".len())
            .any(|row| row == b"nobody (U,W)")
    );
    assert!(
        fixture
            .command(
                &fixture.c,
                &fixture.c_socket,
                &["server-access", "-d", "nobody"],
                None
            )
            .status
            .success()
    );
    assert_eq!(list(&fixture.c_socket).stdout, c.stdout);
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let output = fixture.command(&fixture.c, socket, &["server-access"], None);
        assert!(!output.status.success());
        assert_eq!(output.stderr, b"missing user or group argument\n");
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn scheduler_command_formats_are_canonical_and_do_not_leak_to_nested_items() {
    let fixture = Fixture::start();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let run = |args: &[&str], input| fixture.command(&fixture.c, socket, args, input);
        for (args, expected) in [
            (
                vec!["run", "printf '%s\\n' '#{command}'"],
                b"run-shell\n".as_slice(),
            ),
            (
                vec![
                    "if",
                    "-F",
                    "#{==:#{command},if-shell}",
                    "display -p '#{command}'",
                    "display -p wrong",
                ],
                b"display-message\n",
            ),
            (vec!["run", "-C", "display -p '#{command}'"], b"run-shell\n"),
            (vec!["run", "-C", "display -p extra", "ignored"], b"extra\n"),
        ] {
            let output = run(&args, None);
            assert!(output.status.success(), "{args:?}: {output:?}");
            assert_eq!(output.stdout, expected, "{socket:?}: {args:?}");
        }
        let output = run(
            &["source", "-"],
            Some(b"display -p '#{command}'\n".as_slice()),
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"display-message\n");
        let output = run(
            &[
                "set-hook",
                "-g",
                "after-new-window",
                "set -gF @hook-command '#{command}'",
            ],
            None,
        );
        assert!(output.status.success(), "{output:?}");
        assert!(
            run(&["new-window", "-d", "sleep 30"], None)
                .status
                .success()
        );
        assert_eq!(
            run(&["show-options", "-gv", "@hook-command"], None).stdout,
            b"new-window\n"
        );
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn control_buffer_notifications_cover_replacement_eviction_capture_load_and_paste() {
    use tmuxx_protocol::Notification::{PasteBufferChanged, PasteBufferDeleted};
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let mut control = fixture.control(&fixture.c, socket);
        let mut batches = Vec::new();
        for (args, stdin) in [
            (vec!["capture-pane", "-P"], None),
            (vec!["capture-pane", "-P", "-b", "invalid\x1b"], None),
            (vec!["load-buffer", "-b", "invalid\x1b", "/dev/null"], None),
            (vec!["set-option", "-s", "buffer-limit", "1"], None),
            (vec!["set-buffer", "one"], None),
            (vec!["set-buffer", "two"], None),
            (vec!["set-buffer", "-b", "named", "first"], None),
            (vec!["set-buffer", "-ab", "named", "second"], None),
            (vec!["set-buffer", "-b", "named", "-n", "renamed"], None),
            (vec!["set-buffer", "-b", "line\n\t\\é", "first"], None),
            (
                vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "line\n\t\\é"],
                None,
            ),
            (vec!["set-buffer", "-b", "target\n", "replaced"], None),
            (
                vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "target\n"],
                None,
            ),
            (vec!["load-buffer", "-"], Some(b"from file\0".as_slice())),
            (
                vec!["load-buffer", "-b", "load\n\té", "-"],
                Some(b"named file\0".as_slice()),
            ),
            (vec!["capture-pane", "-t", "test:0.0"], None),
            (
                vec!["capture-pane", "-b", "renamed", "-t", "test:0.0"],
                None,
            ),
            (
                vec!["capture-pane", "-b", "capture\n\t\\", "-t", "test:0.0"],
                None,
            ),
            (
                vec!["paste-buffer", "-d", "-b", "renamed", "-t", "test:0.0"],
                None,
            ),
            (vec!["delete-buffer"], None),
        ] {
            let result = fixture.command(&fixture.c, socket, &args, stdin);
            assert!(result.status.success(), "{args:?}: {result:?}");
            writeln!(
                control.child.stdin.as_mut().unwrap(),
                "display-message -p buffer-barrier"
            )
            .unwrap();
            let mut records = Vec::new();
            let mut barrier = false;
            loop {
                match control.next() {
                    Event::Notification(
                        record @ (PasteBufferChanged { .. } | PasteBufferDeleted { .. }),
                    ) => records.push(record),
                    Event::CommandOutput(line) => {
                        assert_eq!(line, b"buffer-barrier");
                        barrier = true;
                    }
                    Event::End { failed, .. } if barrier => {
                        assert!(!failed);
                        break;
                    }
                    _ => {}
                }
            }
            batches.push(records);
        }
        observations.push(batches);
    }
    assert_eq!(observations[0], observations[1]);
    assert!(observations[0][..3].iter().all(Vec::is_empty));
    assert!(
        observations[0]
            .iter()
            .flatten()
            .any(|record| matches!(record, PasteBufferDeleted { .. }))
    );
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn buffer_names_creation_lookup_rename_and_empty_input_match_c() {
    let fixture = Fixture::start();
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        let mut observations = Vec::new();
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            let mut outputs = Vec::new();
            for args in [
                vec!["capture-pane", "-P"],
                vec!["capture-pane", "-P", "-b", "invalid\x1b"],
                vec!["set-buffer", "-b", "invalid\x1b", ""],
                vec!["load-buffer", "-b", "invalid\x1b", "/dev/null"],
                vec!["set-buffer", "automatic"],
                vec!["set-buffer", "-b", "line\n\t\\é", "first"],
                vec!["show-buffer", "-b", "line\n\t\\é"],
                vec!["show-buffer", "-b", "line\\n\\t\\\\é"],
                vec!["set-buffer", "-ab", "line\n\t\\é", "replacement"],
                vec!["set-buffer", "-ab", "line\\n\\t\\\\é", " appended"],
                vec!["show-buffer", "-b", "line\\n\\t\\\\é"],
                vec!["show-buffer", "-b", "line\\\\n\\\\t\\\\\\\\é"],
                vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "line\n\t\\é"],
                vec!["set-buffer", "-b", "target\n", "old"],
                vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "target\n"],
                vec!["capture-pane", "-b", "capture\n\t\\"],
                vec![
                    "list-buffers",
                    "-F",
                    "#{buffer_name}|#{buffer_size}|#{buffer_sample}",
                ],
            ] {
                let output = fixture.command(client, socket, &args, None);
                outputs.push((output.status.code(), output.stdout, output.stderr));
            }
            observations.push(outputs);
        }
        assert_eq!(observations[0], observations[1], "{client:?}");
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn invalid_buffer_names_are_rejected_with_safe_native_diagnostic_difference() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::start();
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            assert!(
                fixture
                    .command(client, socket, &["set-buffer", "-b", "kept", "old"], None)
                    .status
                    .success()
            );
            assert!(
                fixture
                    .command(
                        client,
                        socket,
                        &["set-buffer", "-b", "�", "valid Unicode"],
                        None
                    )
                    .status
                    .success()
            );
            let before = fixture
                .command(
                    client,
                    socket,
                    &[
                        "list-buffers",
                        "-F",
                        "#{buffer_name}|#{buffer_size}|#{buffer_sample}",
                    ],
                    None,
                )
                .stdout;
            for (args, prefix) in [
                (
                    vec![b"show-buffer".as_slice(), b"-b", b"\xff"],
                    b"no buffer ".as_slice(),
                ),
                (vec![b"delete-buffer", b"-b", b"\xff"], b"unknown buffer: "),
                (
                    vec![b"set-buffer", b"-b", b"\xff", b"-n", b"renamed"],
                    b"unknown buffer: ",
                ),
                (vec![b"paste-buffer", b"-db", b"\xff"], b"no buffer "),
                (vec![b"save-buffer", b"-b", b"\xff", b"-"], b"no buffer "),
            ] {
                let output = Command::new(client)
                    .arg("-N")
                    .args(["-u", "-f", "/dev/null", "-S"])
                    .arg(socket)
                    .args(args.iter().map(|arg| std::ffi::OsStr::from_bytes(arg)))
                    .output()
                    .unwrap();
                assert!(!output.status.success(), "{args:?}");
                assert!(output.stdout.is_empty());
                let name = if socket == &fixture.c_socket {
                    b"\xff\n".as_slice()
                } else {
                    b"\\xff\n"
                };
                assert_eq!(output.stderr, [prefix, name].concat());
                assert_eq!(
                    fixture
                        .command(
                            client,
                            socket,
                            &[
                                "list-buffers",
                                "-F",
                                "#{buffer_name}|#{buffer_size}|#{buffer_sample}"
                            ],
                            None
                        )
                        .stdout,
                    before
                );
            }
            for name in [
                b"bad\r".as_slice(),
                b"bad\x1b",
                b"bad\x7f",
                b"\xff\n%exit",
                b"\xc3",
                b"\xc0\x80",
            ] {
                for args in [
                    vec![b"set-buffer".as_slice(), b"-b", name, b"new"],
                    vec![b"set-buffer", b"-b", b"kept", b"-n", name],
                    vec![b"capture-pane", b"-b", name],
                    vec![b"load-buffer", b"-b", name, b"-"],
                ] {
                    let mut child = Command::new(client)
                        .arg("-N")
                        .args(["-u", "-f", "/dev/null", "-S"])
                        .arg(socket)
                        .args(args.iter().map(|arg| std::ffi::OsStr::from_bytes(arg)))
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap();
                    child.stdin.take().unwrap().write_all(b"new").unwrap();
                    let output = wait(child);
                    assert!(!output.status.success(), "{client:?} {socket:?}: {args:?}");
                    assert!(output.stdout.is_empty());
                    let error = if socket == &fixture.c_socket {
                        [b"invalid buffer name: ".as_slice(), name, b"\n"].concat()
                    } else {
                        format!("invalid buffer name: {}\n", name.escape_ascii()).into_bytes()
                    };
                    assert_eq!(output.stderr, error, "{client:?} {socket:?}: {args:?}");
                    assert_eq!(
                        fixture
                            .command(
                                client,
                                socket,
                                &[
                                    "list-buffers",
                                    "-F",
                                    "#{buffer_name}|#{buffer_size}|#{buffer_sample}"
                                ],
                                None
                            )
                            .stdout,
                        before
                    );
                }
                // An empty read does not validate the unused name.
                let output = Command::new(client)
                    .arg("-N")
                    .args(["-u", "-f", "/dev/null", "-S"])
                    .arg(socket)
                    .args([
                        std::ffi::OsStr::new("load-buffer"),
                        std::ffi::OsStr::new("-b"),
                        std::ffi::OsStr::from_bytes(name),
                        std::ffi::OsStr::new("/dev/null"),
                    ])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
            }
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference"]
fn clap_driver_help_and_version_are_explicit_implementation_differences() {
    let fixture = Fixture::start();
    for flag in ["-h", "--help"] {
        let c = fixture.command(&fixture.c, &fixture.c_socket, &[flag], None);
        let rust = fixture.command(
            Path::new(env!("CARGO_BIN_EXE_tmuxx")),
            &fixture.rust_socket,
            &[flag],
            None,
        );
        assert_eq!(c.status.success(), flag == "-h");
        assert!(
            [c.stdout.as_slice(), c.stderr.as_slice()]
                .concat()
                .windows(b"usage:".len())
                .any(|value| value == b"usage:")
        );
        assert!(rust.status.success());
        assert!(rust.stderr.is_empty());
        assert!(
            rust.stdout
                .windows(b"Headless tmux-compatible".len())
                .any(|value| value == b"Headless tmux-compatible")
        );
    }
    let c = fixture.command(&fixture.c, &fixture.c_socket, &["-V"], None);
    let rust = fixture.command(
        Path::new(env!("CARGO_BIN_EXE_tmuxx")),
        &fixture.rust_socket,
        &["-V"],
        None,
    );
    assert!(c.status.success() && rust.status.success());
    assert!(c.stdout.starts_with(b"tmux "));
    assert_eq!(
        rust.stdout,
        format!("tmuxx {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn server_diagnostics_expose_native_logs_jobs_limits_and_c_empty_tty_state() {
    let fixture = Fixture::start();
    for socket in [&fixture.c_socket, &fixture.rust_socket] {
        fixture.assert_owner(socket);
        let run = |args: &[&str]| fixture.command(&fixture.c, socket, args, None);
        for flags in ["-T", "-J", "-JT"] {
            let result = run(&["show-messages", flags]);
            assert!(result.status.success(), "{result:?}");
            assert!(result.stdout.is_empty());
        }
        let missing = run(&["show-messages", "-JT", "-t", "not-a-client"]);
        assert!(missing.status.success());
        assert!(missing.stdout.is_empty() && missing.stderr.is_empty());
        assert!(run(&["run-shell", "-b", "sleep 30"]).status.success());
        let jobs = run(&["show-messages", "-JT"]);
        assert!(jobs.status.success(), "{jobs:?}");
        assert!(jobs.stdout.starts_with(b"Job 0: "));
        assert!(
            jobs.stdout
                .windows(b"sleep 30".len())
                .any(|part| part == b"sleep 30")
        );
        if socket == &fixture.rust_socket {
            assert!(jobs.stdout.ends_with(b"kind=shell]\n"));
            let long_job = format!("sleep 30 #{}", "\t".repeat(5000));
            assert!(run(&["run-shell", "-b", &long_job]).status.success());
            let summaries = run(&["show-messages", "-J"]);
            assert!(summaries.status.success());
            let long = summaries
                .stdout
                .split(|byte| *byte == b'\n')
                .find(|line| {
                    line.windows(b"[truncated]".len())
                        .any(|part| part == b"[truncated]")
                })
                .unwrap();
            assert!(long.len() < 4200);
            assert!(long.ends_with(b"kind=shell]"));
        } else {
            assert!(
                jobs.stdout
                    .windows(b"[fd=".len())
                    .any(|part| part == b"[fd=")
            );
        }
        let messages = run(&["show-messages"]);
        assert!(messages.status.success());
        assert!(!messages.stdout.is_empty());
        assert!(run(&["set", "-s", "message-limit", "0"]).status.success());
        assert!(
            run(&["display-message", "-p", "discarded-log"])
                .status
                .success()
        );
        assert!(run(&["show-messages"]).stdout.is_empty());
        assert!(run(&["set", "-s", "message-limit", "2"]).status.success());
        for text in ["log-first", "log-second", "log-third"] {
            assert!(run(&["display-message", "-p", text]).status.success());
        }
        let messages = run(&["show-messages"]);
        assert!(messages.status.success());
        assert!(
            messages
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count()
                <= 2
        );
        if socket == &fixture.rust_socket {
            let lines = String::from_utf8(messages.stdout)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(lines.len(), 2);
            assert!(lines[0].contains("log-third") && lines[1].contains("log-second"));
            assert!(!run(&["select-pane", "-t", "%999999999"]).status.success());
            assert!(
                String::from_utf8(run(&["show-messages"]).stdout)
                    .unwrap()
                    .contains("error:")
            );
            let long = "x".repeat(5_000);
            assert!(run(&["display-message", "-p", &long]).status.success());
            let messages = run(&["show-messages"]);
            assert!(messages.stdout.len() < 9000);
            assert!(
                messages
                    .stdout
                    .windows(b"[truncated]".len())
                    .any(|part| part == b"[truncated]")
            );
        }
        let limit = run(&["set", "-s", "message-limit", "16385"]);
        if socket == &fixture.rust_socket {
            assert!(!limit.status.success());
            assert_eq!(limit.stderr, b"message-limit exceeds native limit: 16384\n");
            assert_eq!(run(&["show", "-sv", "message-limit"]).stdout, b"2\n");
        } else {
            assert!(limit.status.success());
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn load_buffer_limit_is_explicit_preserves_buffers_and_recovers_file_streams() {
    let fixture = Fixture::start();
    let limit = tmuxx_runtime::MAX_REQUEST_BYTES;
    let boundary = fixture.directory.join("buffer-limit");
    let excessive = fixture.directory.join("buffer-excessive");
    fs::File::create(&boundary)
        .unwrap()
        .set_len(limit as u64)
        .unwrap();
    fs::File::create(&excessive)
        .unwrap()
        .set_len(limit as u64 + 1)
        .unwrap();
    let local = fixture.host.as_ref().unwrap().connect().unwrap();
    let native = |args: &[&str]| {
        local
            .send(tmuxx_protocol::native::Request {
                request_id: 1,
                argv: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
                stdin: None,
                subscribe: false,
                control: false,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(tmuxx_protocol::native::ServerMessage::Reply(reply)) = local.try_recv() {
                break reply;
            }
            assert!(Instant::now() < deadline, "native file command timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    assert_eq!(
        native(&[
            "load-buffer",
            "-b",
            "native-boundary",
            boundary.to_str().unwrap()
        ])
        .error,
        None
    );
    assert_eq!(
        native(&[
            "list-buffers",
            "-f",
            "#{==:#{buffer_name},native-boundary}",
            "-F",
            "#{buffer_size}"
        ])
        .output,
        format!("{limit}\n").as_bytes()
    );
    assert_eq!(
        native(&["set-buffer", "-b", "native-kept", "original"]).error,
        None
    );
    let rejected = native(&[
        "load-buffer",
        "-b",
        "native-kept",
        excessive.to_str().unwrap(),
        ";",
        "display-message",
        "-p",
        "AFTER",
    ]);
    assert_eq!(
        rejected.error.as_deref(),
        Some("buffer exceeds input size limit")
    );
    assert!(rejected.output.is_empty());
    assert_eq!(
        native(&["show-buffer", "-b", "native-kept"]).output,
        b"original"
    );
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        for socket in [&fixture.c_socket, &fixture.rust_socket] {
            fixture.assert_owner(socket);
            let run = |args: &[&str]| fixture.command(client, socket, args, None);
            assert!(
                run(&["load-buffer", "-b", "boundary", boundary.to_str().unwrap()])
                    .status
                    .success()
            );
            assert_eq!(
                run(&[
                    "list-buffers",
                    "-f",
                    "#{==:#{buffer_name},boundary}",
                    "-F",
                    "#{buffer_size}"
                ])
                .stdout,
                format!("{limit}\n").as_bytes()
            );
            assert!(
                run(&["set-buffer", "-b", "kept", "original"])
                    .status
                    .success()
            );
            let result = run(&[
                "load-buffer",
                "-b",
                "kept",
                excessive.to_str().unwrap(),
                ";",
                "display-message",
                "-p",
                "AFTER",
            ]);
            if socket == &fixture.rust_socket {
                assert!(!result.status.success(), "{result:?}");
                assert!(result.stdout.is_empty(), "{result:?}");
                assert_eq!(
                    result.stderr,
                    format!("file input exceeds limit: {}\n", excessive.display()).as_bytes()
                );
                assert_eq!(run(&["show-buffer", "-b", "kept"]).stdout, b"original");
            } else {
                assert!(result.status.success(), "{result:?}");
                assert_eq!(result.stdout, b"AFTER\n");
                assert_eq!(
                    run(&[
                        "list-buffers",
                        "-f",
                        "#{==:#{buffer_name},kept}",
                        "-F",
                        "#{buffer_size}"
                    ])
                    .stdout,
                    format!("{}\n", limit + 1).as_bytes()
                );
            }
            assert_eq!(
                run(&["display-message", "-p", "RECOVERED"]).stdout,
                b"RECOVERED\n"
            );
            assert!(
                run(&["set-buffer", "-b", "kept", "original"])
                    .status
                    .success()
            );
            let input = vec![b'z'; limit + 1];
            let result = fixture.command(
                client,
                socket,
                &["load-buffer", "-b", "kept", "-"],
                Some(&input),
            );
            if socket == &fixture.rust_socket {
                assert!(!result.status.success(), "{result:?}");
                assert_eq!(result.stderr, b"file input exceeds limit: -\n");
                assert_eq!(run(&["show-buffer", "-b", "kept"]).stdout, b"original");
            } else {
                assert!(result.status.success(), "{result:?}");
                assert_eq!(
                    run(&[
                        "list-buffers",
                        "-f",
                        "#{==:#{buffer_name},kept}",
                        "-F",
                        "#{buffer_size}"
                    ])
                    .stdout,
                    format!("{}\n", limit + 1).as_bytes()
                );
            }
        }
    }
    // WAIT closes its guard first; a later file error is unframed, as in C.
    for client in [&fixture.c, Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        let mut control = fixture.control(client, &fixture.rust_socket);
        writeln!(
            control.child.stdin.as_mut().unwrap(),
            "load-buffer -b kept {}\ndisplay-message -p CONTROL-RECOVERED",
            excessive.display()
        )
        .unwrap();
        let mut error = Vec::new();
        let mut recovered = false;
        loop {
            match control.next() {
                Event::UnframedOutput(line) => error.push(line),
                Event::CommandOutput(line) => {
                    assert_eq!(line, b"CONTROL-RECOVERED");
                    recovered = true;
                }
                Event::End { failed, .. } => {
                    assert!(!failed);
                    if recovered {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(
            error,
            [format!("file input exceeds limit: {}", excessive.display()).into_bytes()]
        );
    }
}

#[test]
#[ignore = "requires an explicit C reference and real Unix sockets/PTYs"]
fn native_and_unix_control_preserve_c_command_guards_blocks_and_file_callbacks() {
    for native in [false, true] {
        let fixture = Fixture::start();
        let mut c = fixture.control(&fixture.c, &fixture.c_socket);
        let mut rust = if native {
            ControlStream::native(&fixture.directory.join("native"))
        } else {
            fixture.control(Path::new(env!("CARGO_BIN_EXE_tmuxx")), &fixture.rust_socket)
        };
        c.records("display -p ready");
        rust.records("display -p ready");
        for command in [
            "setenv -g CONTROL_PREP parsed",
            "if-shell -F 1 { setenv -g CONTROL_PREP changed; display -p \"block:$CONTROL_PREP\"; if-shell -F 1 'display -p \"string:$CONTROL_PREP\"' }",
            "set -s command-alias[90] 'late=display-message -p original'",
            "if-shell -F 1 { set -s command-alias[90] 'late=display-message -p changed'; if-shell -F 1 { late }; if-shell -F 1 'late' }",
            "run-shell -C -d 0.01 { set -g @value after; display -p 'block:#{@value}:#{command}' }",
            "if-shell true { display -p async-if }",
            "run-shell 'printf job; exit 3'",
            "set-hook -g after-rename-window { run-shell -C { display -p hook } }",
            "rename-window -t test:0 renamed",
            "set-hook -gu after-rename-window",
            "if-shell -F 0 { no-such-command }",
            "source-file -",
            "load-buffer -b missing /nonexistent/tmuxx-control-buffer; display -p after-file-error",
            "load-buffer -b stdin -; display -p after-stdin-error",
        ] {
            assert_eq!(rust.records(command), c.records(command), "{command}");
        }
        let path = fixture.directory.join("control.conf");
        for source in [
            "display -p first\nif-shell -F 1 { display -p nested }\ndisplay -p last\n",
            "not-a-command\n",
            "display -p first; source-file /nonexistent/tmuxx-control-source; display -p following\n",
        ] {
            fs::write(&path, source).unwrap();
            let command = format!("source-file '{}'", path.display());
            assert_eq!(rust.records(&command), c.records(&command), "{source}");
        }
        fs::write(&path, "display -p surviving-file\n").unwrap();
        let command = format!(
            "source-file /nonexistent/tmuxx-control-source '{}' ; display -p after-source",
            path.display()
        );
        assert_eq!(rust.records(&command), c.records(&command));
        let buffer = fixture.directory.join("control-buffer");
        fs::write(&buffer, b"buffer line\n").unwrap();
        for command in [
            format!("load-buffer -b loaded '{}'", buffer.display()),
            "save-buffer -b loaded -".into(),
            "delete-buffer -b loaded".into(),
        ] {
            assert_eq!(rust.records(&command), c.records(&command), "{command}");
        }
        let background =
            "run-shell -b -C -d 0.01 { set -g @background done; display -p background-done }";
        assert_eq!(
            rust.records_until(background, "background-done", false),
            c.records_until(background, "background-done", false)
        );
        for (side, client) in [("Rust", &mut rust), ("C", &mut c)] {
            writeln!(client.child.stdin.as_mut().unwrap()).unwrap();
            let event = client
                .events
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|error| panic!("{side} native={native} empty-line exit: {error}"));
            assert!(
                matches!(
                    event,
                    Event::Notification(tmuxx_protocol::Notification::Exit { .. })
                ),
                "{side} native={native}: {event:?}"
            );
        }
    }
}
