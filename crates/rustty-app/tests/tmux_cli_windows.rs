//! Exercise the executable used by both the GUI daemon launcher and SSH servers.
//! Build the Windows bundle first; RUSTTY_CONPTY_RESOURCES selects its resources.
#![cfg(windows)]

use rustty::tmuxx::{Connection, windows_endpoint};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tmuxx_protocol::native::{Hello, Request, ServerMessage, SnapshotAssembler};

fn executable() -> PathBuf {
    std::env::var_os("RUSTTY_TEST_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_rustty")))
}
const TIMEOUT: Duration = Duration::from_secs(15);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn output(command: &mut Command) -> Output {
    let mut child = OwnedChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let collect = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = collect(Box::new(child.0.stdout.take().unwrap()));
    let stderr = collect(Box::new(child.0.stderr.take().unwrap()));
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "rustty tmux command timed out");
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

struct Server {
    owner: OwnedChild,
    label: String,
    endpoint: PathBuf,
    resources: PathBuf,
    instance: String,
}

impl Server {
    fn command_with(resources: &Path) -> Command {
        let mut command = Command::new(executable());
        command
            .arg("tmux")
            .creation_flags(CREATE_NO_WINDOW)
            .env("TMUXX_CONPTY_RESOURCES", resources)
            .env_remove("TMUX")
            .env_remove("TMUXX_ENDPOINT");
        command
    }

    fn new() -> Self {
        let resources = std::env::var_os("RUSTTY_CONPTY_RESOURCES")
            .or_else(|| std::env::var_os("TMUXX_CONPTY_RESOURCES"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
                ["debug", "release"]
                    .into_iter()
                    .map(|profile| workspace.join(format!("target/{profile}/Rustty/resources")))
                    .find(|path| path.join("conpty").is_dir())
                    .expect("build the Windows bundle or set RUSTTY_CONPTY_RESOURCES")
            });
        let label = format!(
            "rustty-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let endpoint = windows_endpoint(&label).unwrap();

        // Windows requires an explicit owner; a client must not create one.
        let missing = output(
            Self::command_with(&resources)
                .arg("--native-endpoint")
                .arg(&endpoint)
                .arg("list-sessions"),
        );
        assert!(!missing.status.success(), "{missing:?}");
        assert!(Connection::native(&endpoint, Arc::new(|| {})).is_err());

        // The helper uses current_exe, which is the test runner here. Exercise its
        // production arguments with the actual Rustty executable instead.
        let prepared = tmuxx_cli::native_daemon_command(&endpoint, &["tmux"]).unwrap();
        let owner = OwnedChild(
            Command::new(executable())
                .args(prepared.get_args())
                .args(["-f", "NUL"])
                .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
                .env("TMUXX_CONPTY_RESOURCES", &resources)
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let mut server = Self {
            owner,
            label,
            endpoint,
            resources,
            instance: String::new(),
        };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(connection) = Connection::native(&server.endpoint, Arc::new(|| {})) {
                while connection.graph().is_none() {
                    assert!(!connection.is_closed(), "{:?}", connection.errors());
                    assert!(Instant::now() < deadline, "owner snapshot did not arrive");
                    std::thread::sleep(Duration::from_millis(10));
                }
                server.instance = connection.instance();
                assert!(!server.instance.is_empty());
                break;
            }
            assert!(
                server.owner.0.try_wait().unwrap().is_none(),
                "owner exited during startup"
            );
            assert!(Instant::now() < deadline, "owner did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        server
    }

    fn command(&self, args: &[&str]) -> Vec<u8> {
        let result = output(
            Self::command_with(&self.resources)
                .arg("--native-endpoint")
                .arg(&self.endpoint)
                .args(args),
        );
        assert!(result.status.success(), "{args:?}: {result:?}");
        result.stdout
    }
}

struct Bridge {
    _child: OwnedChild,
    input: ChildStdin,
    records: mpsc::Receiver<Vec<u8>>,
}

impl Bridge {
    fn new(server: &Server, explicit_endpoint: bool) -> Self {
        let mut command = Server::command_with(&server.resources);
        command.arg("--native-stdio");
        if explicit_endpoint {
            command.arg("--native-endpoint").arg(&server.endpoint);
        }
        let mut child = OwnedChild(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let input = child.0.stdin.take().unwrap();
        let stdout = child.0.stdout.take().unwrap();
        let (send, records) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut record = Vec::new();
                if reader.read_until(b'\n', &mut record).unwrap_or(0) == 0
                    || send.send(record).is_err()
                {
                    break;
                }
            }
        });
        let mut bridge = Self {
            _child: child,
            input,
            records,
        };
        bridge.send(&Hello {
            instance: (!explicit_endpoint).then(|| server.label.clone()),
            ..Hello::default()
        });
        let hello: Hello = serde_json::from_slice(&bridge.record()).unwrap();
        assert_eq!(hello.version, Hello::default().version);
        assert_eq!(hello.instance_id.as_deref(), Some(server.instance.as_str()));
        bridge
    }

    fn send(&mut self, value: &impl serde::Serialize) {
        serde_json::to_writer(&mut self.input, value).unwrap();
        self.input.write_all(b"\n").unwrap();
        self.input.flush().unwrap();
    }

    fn record(&self) -> Vec<u8> {
        self.records
            .recv_timeout(TIMEOUT)
            .expect("native stdio response")
    }

    fn command(&mut self, argv: &[&str]) -> Vec<u8> {
        self.send(&Request {
            request_id: 1,
            argv: argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            stdin: None,
            subscribe: false,
            control: false,
        });
        match serde_json::from_slice::<ServerMessage>(&self.record()).unwrap() {
            ServerMessage::Reply(reply) => {
                assert_eq!(reply.request_id, 1);
                assert!(reply.error.is_none(), "{reply:?}");
                reply.output
            }
            other => panic!("unexpected native response: {other:?}"),
        }
    }

    fn subscribe(&mut self, server: &Server) {
        self.send(&Request {
            request_id: 2,
            argv: Vec::new(),
            stdin: None,
            subscribe: true,
            control: false,
        });
        let deadline = Instant::now() + TIMEOUT;
        let mut snapshots = SnapshotAssembler::default();
        loop {
            let bytes = self
                .records
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("native subscription snapshot");
            match snapshots
                .push(serde_json::from_slice(&bytes).unwrap())
                .unwrap()
            {
                Some(ServerMessage::Snapshot {
                    instance_id, state, ..
                }) => {
                    assert_eq!(instance_id, server.instance);
                    assert!(
                        state["sessions"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|session| session["name"] == "proof")
                    );
                    assert!(!state["panes"].as_array().unwrap().is_empty());
                    return;
                }
                Some(ServerMessage::Reply(reply)) => assert!(reply.error.is_none(), "{reply:?}"),
                _ => {}
            }
        }
    }

    fn close_input(self) {
        let Self {
            _child: mut child,
            input,
            ..
        } = self;
        drop(input);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "stdio bridge failed after EOF: {status}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "stdio bridge did not exit after EOF"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "requires the pinned Windows ConPTY bundle; run with --ignored"]
fn tmux_daemon_and_ssh_stdio_keep_the_same_session_after_clients_exit() {
    let mut server = Server::new();
    let shell = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32")
        .join("cmd.exe");
    server.command(&[
        "new-session",
        "-d",
        "-s",
        "proof",
        shell.to_str().unwrap(),
        "/D",
        "/Q",
        "/K",
        "echo RUSTTY_CLI_READY",
    ]);
    let query = ["list-panes", "-t", "proof:", "-F", "#{pane_pid}"];
    let pid = server.command(&query);
    assert!(String::from_utf8_lossy(&pid).trim().parse::<u32>().unwrap() > 0);
    assert_eq!(
        server.command(&["list-sessions", "-F", "#{session_name}"]),
        b"proof\n"
    );

    // Both the GUI's explicit pipe and an SSH client's Hello selector reach the
    // same daemon. Closing either bridge must only detach that client.
    for explicit_endpoint in [true, false] {
        let mut bridge = Bridge::new(&server, explicit_endpoint);
        assert_eq!(bridge.command(&query), pid);
        bridge.subscribe(&server);
        if explicit_endpoint {
            drop(bridge);
        } else {
            bridge.close_input();
        }
        assert!(server.owner.0.try_wait().unwrap().is_none());
        assert_eq!(server.command(&query), pid);
    }
    server.command(&[
        "send-keys",
        "-t",
        "proof:",
        "-l",
        "echo RUSTTY-STDIO-PERSISTED",
    ]);
    server.command(&["send-keys", "-t", "proof:", "Enter"]);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let capture = server.command(&["capture-pane", "-p", "-t", "proof:"]);
        if String::from_utf8_lossy(&capture)
            .lines()
            .any(|line| line.trim() == "RUSTTY-STDIO-PERSISTED")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "original shell did not receive input"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.command(&query), pid);
    server.command(&["kill-server"]);
    let deadline = Instant::now() + TIMEOUT;
    while server.owner.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "owner did not shut down");
        std::thread::sleep(Duration::from_millis(10));
    }
}
