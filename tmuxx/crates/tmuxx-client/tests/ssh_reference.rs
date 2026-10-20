#![cfg(any(unix, windows))]
//! Opt-in actual SSH to a pinned stock C tmux, with owned private fixtures.
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tmuxx_client::system::{SystemClient, SystemEvent};
use tmuxx_command::Command as TmuxCommand;

#[cfg(target_os = "linux")]
#[path = "../../../test-support/socket_owner.rs"]
mod socket_owner;

#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote;

#[cfg(unix)]
struct Fixture {
    directory: PathBuf,
    tmux: PathBuf,
    ssh: PathBuf,
    socket: PathBuf,
    port: u16,
    user: String,
    daemon: Option<Child>,
    #[cfg(target_os = "linux")]
    owner: Option<socket_owner::SocketOwner>,
}
#[cfg(unix)]
impl Fixture {
    fn start() -> Self {
        let directory = std::env::temp_dir().join(format!("tmuxx-ssh-{}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let tmux = std::env::var_os("TMUXX_C_TMUX")
            .expect("set TMUXX_C_TMUX")
            .into();
        let ssh = std::env::var_os("TMUXX_SSH")
            .unwrap_or_else(|| "/usr/bin/ssh".into())
            .into();
        let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut fixture = Self {
            socket: directory.join("tmux"),
            directory,
            tmux,
            ssh,
            port,
            user,
            daemon: None,
            #[cfg(target_os = "linux")]
            owner: None,
        };
        for name in ["host", "client"] {
            let result = Command::new(
                std::env::var_os("TMUXX_SSH_KEYGEN")
                    .unwrap_or_else(|| "/usr/bin/ssh-keygen".into()),
            )
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(fixture.directory.join(name))
            .output()
            .unwrap();
            assert!(
                result.status.success(),
                "ssh-keygen: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let host_key = fs::read_to_string(fixture.directory.join("host.pub")).unwrap();
        let parts = host_key.split_whitespace().collect::<Vec<_>>();
        fs::write(
            fixture.directory.join("known_hosts"),
            format!("[127.0.0.1]:{port} {} {}\n", parts[0], parts[1]),
        )
        .unwrap();
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {dir}/host\nPidFile {dir}/pid\nAuthorizedKeysFile {dir}/client.pub\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nAllowUsers {user}\nLogLevel ERROR\nPermitTTY no\nAllowTcpForwarding no\nX11Forwarding no\n",
            dir = fixture.directory.display(),
            user = fixture.user
        );
        fs::write(fixture.directory.join("sshd.conf"), config).unwrap();
        let daemon =
            Command::new(std::env::var_os("TMUXX_SSHD").unwrap_or_else(|| "/usr/sbin/sshd".into()))
                .args(["-D", "-e", "-f"])
                .arg(fixture.directory.join("sshd.conf"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(fixture.directory.join("sshd.log")).unwrap())
                .spawn()
                .expect("sshd must be installed");
        fixture.daemon = Some(daemon);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let probe = fixture.ssh_command("true").output().unwrap();
            if probe.status.success() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "SSH fixture failed: {}\n{}",
                String::from_utf8_lossy(&probe.stderr),
                fs::read_to_string(fixture.directory.join("sshd.log")).unwrap()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let result = fixture.run_unchecked(&["new-session", "-d", "-s", "test", "-x", "80", "-y", "24", "/bin/sh", "-c", "printf 'READY\\n'; while IFS= read -r value; do printf '\\001\\377:%s\\n' \"$value\"; done"], false);
        assert!(
            result.status.success(),
            "remote tmux start: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        #[cfg(target_os = "linux")]
        {
            fixture.owner = Some(
                socket_owner::SocketOwner::capture(&fixture.socket, &fixture.tmux, None).unwrap(),
            );
        }
        fixture
    }
    fn ssh_command(&self, command: &str) -> Command {
        let mut ssh = Command::new(&self.ssh);
        ssh.args(["-T", "-F", "/dev/null", "-p"])
            .arg(self.port.to_string())
            .arg("-i")
            .arg(self.directory.join("client"))
            .args([
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "ConnectTimeout=2",
                "-o",
                "ControlMaster=no",
                "-o",
            ])
            .arg(format!(
                "UserKnownHostsFile={}",
                self.directory.join("known_hosts").display()
            ))
            .arg(format!("{}@127.0.0.1", self.user))
            .arg(command);
        ssh
    }
    fn command_text(&self, args: &[&str], no_start: bool) -> String {
        let mut words = vec![
            self.tmux.to_str().unwrap(),
            "-f",
            "/dev/null",
            "-S",
            self.socket.to_str().unwrap(),
        ];
        if no_start {
            words.push("-N");
        }
        words.extend_from_slice(args);
        format!(
            "exec {}",
            words.into_iter().map(quote).collect::<Vec<_>>().join(" ")
        )
    }
    fn assert_owner(&self) {
        #[cfg(target_os = "linux")]
        self.owner
            .as_ref()
            .expect("SSH C bootstrap must finish first")
            .check()
            .unwrap();
    }
    fn run_unchecked(&self, args: &[&str], no_start: bool) -> Output {
        self.ssh_command(&self.command_text(args, no_start))
            .output()
            .unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        self.assert_owner();
        self.run_unchecked(args, true)
    }
    fn connect(&self, nonce: &[u8]) -> Remote {
        self.assert_owner();
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stderr_path = self.directory.join(format!(
            "control-{}.stderr",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        connect_command(
            self.ssh_command(&self.command_text(&["-C", "attach-session", "-t", "test"], true)),
            nonce,
            stderr_path,
        )
    }
}

#[cfg(windows)]
struct Fixture {
    reference: remote::RemoteReference,
    selection: PathBuf,
    directory: PathBuf,
}

#[cfg(windows)]
impl Fixture {
    fn start() -> Self {
        let reference = remote::RemoteReference::new();
        let selection = std::env::var_os("TMUXX_SYSTEM_C_SELECTION")
            .expect("set TMUXX_SYSTEM_C_SELECTION to the private fixed SSH selection file")
            .into();
        let directory = std::env::temp_dir().join(format!(
            "tmuxx-ssh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let fixture = Self {
            directory,
            reference,
            selection,
        };
        // The forced SSH entry independently validates the selected binary,
        // socket, owner, server start ticks and short lease. It never starts C.
        fs::write(&fixture.selection, fixture.reference.identity()).unwrap();
        let result = fixture.run(&["new-session", "-d", "-s", "test", "-x", "80", "-y", "24", "/bin/sh", "-c", "printf 'READY\\n'; while IFS= read -r value; do printf '\\001\\377:%s\\n' \"$value\"; done"]);
        assert!(
            result.status.success(),
            "remote C fixture start: {result:?}"
        );
        fixture
    }

    fn run(&self, args: &[&str]) -> Output {
        self.reference.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        )
    }

    fn ssh_command(&self, source: &str) -> Command {
        let ssh = std::env::var_os("TMUXX_SYSTEM_C_SSH")
            .expect("set TMUXX_SYSTEM_C_SSH to the identified native Windows SSH executable");
        let config = std::env::var_os("TMUXX_SYSTEM_C_SSH_CONFIG")
            .expect("set TMUXX_SYSTEM_C_SSH_CONFIG to the restricted SSH fixture config");
        let host = std::env::var_os("TMUXX_SYSTEM_C_SSH_HOST")
            .expect("set TMUXX_SYSTEM_C_SSH_HOST to the explicit fixture alias");
        let mut command = Command::new(ssh);
        command
            .args(["-T", "-F"])
            .arg(config)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "ControlMaster=no",
                "--",
            ])
            .arg(host)
            .arg(source);
        command
    }

    fn run_unchecked(&self, args: &[&str], no_start: bool) -> Output {
        assert!(
            no_start,
            "the fixed Windows SSH entry cannot start an owner"
        );
        let source = if args == ["-C", "attach-session", "-t", "test"] {
            "exec tmux -C attach-session -f new-layouts -t '$0'".to_owned()
        } else {
            // Startup requests deliberately reach the fixed entry's deny path.
            // Selection and the supported attachment form remain protocol data.
            format!(
                "exec tmux {}",
                args.iter()
                    .map(|arg| quote(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        self.ssh_command(&source).output().unwrap()
    }

    fn connect(&self, nonce: &[u8]) -> Remote {
        let command = self.ssh_command("exec tmux -C attach-session -f new-layouts -t '$0'");
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stderr_path = self.directory.join(format!(
            "control-{}.stderr",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        connect_command(command, nonce, stderr_path)
    }
}

#[cfg(windows)]
impl Drop for Fixture {
    fn drop(&mut self) {
        if fs::read(&self.selection).ok().as_deref() == Some(self.reference.identity().as_slice()) {
            let _ = fs::remove_file(&self.selection);
        }
        if std::thread::panicking() {
            eprintln!("Preserved SSH transcripts: {}", self.directory.display());
        } else {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

fn connect_command(mut command: Command, nonce: &[u8], stderr_path: PathBuf) -> Remote {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(&stderr_path).unwrap())
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut chunk = [0; 8192];
        while let Ok(n) = output.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if tx.send(chunk[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut model = SystemClient::new(nonce.to_vec()).unwrap();
    model.bootstrap().unwrap();
    Remote {
        child: Some(child),
        input: Some(input),
        reader: Some(reader),
        rx,
        model,
        stderr_path,
    }
}
#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        let cleanup_error = self
            .owner
            .as_ref()
            .and_then(|owner| owner.terminate().err());
        #[cfg(not(target_os = "linux"))]
        let _ = Command::new(&self.tmux)
            .args(["-N", "-S"])
            .arg(&self.socket)
            .arg("kill-server")
            .output();
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        #[cfg(target_os = "linux")]
        let preserve = cleanup_error.is_some()
            || self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.endpoint_replaced().unwrap_or(true));
        #[cfg(not(target_os = "linux"))]
        let preserve = false;
        if preserve || std::thread::panicking() {
            eprintln!("Preserved SSH transcripts: {}", self.directory.display());
        } else {
            let _ = fs::remove_dir_all(&self.directory);
        }
        #[cfg(target_os = "linux")]
        if let Some(error) = cleanup_error {
            eprintln!("SSH C fixture cleanup: {error}");
            assert!(
                std::thread::panicking(),
                "SSH C fixture cleanup failed: {error}"
            );
        }
    }
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct Remote {
    child: Option<Child>,
    input: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    rx: Receiver<Vec<u8>>,
    model: SystemClient,
    stderr_path: PathBuf,
}
impl Remote {
    fn write_pending(&mut self) {
        for bytes in self.model.take_writes() {
            self.input.as_mut().unwrap().write_all(&bytes).unwrap();
        }
    }
    fn until(&mut self, mut done: impl FnMut(&SystemEvent) -> bool) -> Vec<SystemEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        loop {
            self.write_pending();
            let bytes = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "SSH control response timed out/disconnected: {error}; stderr: {}",
                        String::from_utf8_lossy(&fs::read(&self.stderr_path).unwrap_or_default())
                    )
                });
            let mut found = false;
            // Exercise framing independently of SSH's actual packet boundaries.
            for chunk in bytes.chunks(7) {
                for event in self.model.feed(chunk) {
                    assert!(
                        !matches!(event, SystemEvent::Error(_)),
                        "{event:?}; input {:?}",
                        String::from_utf8_lossy(&bytes)
                    );
                    found |= done(&event);
                    all.push(event);
                }
            }
            if found {
                return all;
            }
        }
    }
    fn command(&mut self, args: &[&str]) -> Vec<u8> {
        let id = self
            .model
            .queue(vec![TmuxCommand {
                blocks: Default::default(),
                args: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            }])
            .unwrap();
        self.until(|event| matches!(event, SystemEvent::CommandFinished { request_id, .. } if *request_id == id)).into_iter()
            .find_map(|event| match event { SystemEvent::CommandFinished { request_id, output, failed } if request_id == id => { assert!(!failed); Some(output) }, _ => None }).unwrap()
    }
    fn close(&mut self) -> Vec<SystemEvent> {
        drop(self.input.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.model.finish()
    }
}
impl Drop for Remote {
    fn drop(&mut self) {
        self.close();
    }
}

#[test]
#[ignore = "requires explicit C tmux, ssh/sshd/keygen, loopback sockets and PTYs"]
fn ssh_stock_tmux_discovery_bytes_reconnect_and_ambiguous_mutation() {
    let fixture = Fixture::start();
    let mut first = fixture.connect(b"ssh-test-first-unique-nonce");
    let bootstrap =
        first.until(|event| matches!(event, SystemEvent::PaneCapture { pane_id: 0, .. }));
    assert!(bootstrap.iter().any(|e| matches!(e, SystemEvent::PaneCapture { data, .. } if data.windows(5).any(|w| w == b"READY"))));
    assert_eq!(first.model.state().sessions.len(), 1);
    let pid = first.command(&["display-message", "-p", "-t", "%0", "#{pane_pid}"]);
    assert!(!pid.is_empty());
    first
        .model
        .queue(vec![TmuxCommand {
            blocks: Default::default(),
            args: ["send-keys", "-t", "%0", "-H", "53", "53", "48", "0d"]
                .map(|a| a.as_bytes().to_vec())
                .to_vec(),
        }])
        .unwrap();
    let mut bytes = Vec::new();
    first.until(|event| {
        if let SystemEvent::PaneOutput { pane_id: 0, data } = event {
            bytes.extend_from_slice(data);
        }
        bytes.windows(6).any(|w| w == b"\x01\xff:SSH")
    });
    // Drain a known read-only barrier before sending the deliberately unobserved mutation.
    assert_eq!(
        first.command(&["display-message", "-p", "ready-to-disconnect"]),
        b"ready-to-disconnect\n"
    );
    let ambiguous = first
        .model
        .queue(vec![TmuxCommand {
            blocks: Default::default(),
            args: ["new-window", "-d", "-n", "ambiguous", "sleep 30"]
                .map(|a| a.as_bytes().to_vec())
                .to_vec(),
        }])
        .unwrap();
    first.write_pending();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let windows = fixture.run(&["list-windows", "-t", "test", "-F", "#{window_name}"]);
        if windows
            .stdout
            .split(|b| *b == b'\n')
            .any(|line| line == b"ambiguous")
        {
            break;
        }
        assert!(Instant::now() < deadline, "mutation was not sent");
        std::thread::sleep(Duration::from_millis(10));
    }
    let unresolved = first.close();
    assert!(unresolved.contains(&SystemEvent::Indeterminate {
        request_id: ambiguous
    }));
    let mut second = fixture.connect(b"ssh-test-second-unique-nonce");
    second.until(|event| matches!(event, SystemEvent::PaneCapture { pane_id: 0, .. }));
    assert_eq!(
        second.command(&["display-message", "-p", "-t", "%0", "#{pane_pid}"]),
        pid
    );
    let windows = second.command(&["list-windows", "-t", "test", "-F", "#{window_name}"]);
    assert_eq!(
        windows
            .split(|b| *b == b'\n')
            .filter(|line| *line == b"ambiguous")
            .count(),
        1
    );
    assert_eq!(second.model.state().panes.len(), 2);
    second.close();
    fixture.run(&["kill-server"]);
    for args in [
        vec!["new-session", "-d", "-s", "fallback", "sleep 30"],
        vec!["-C", "attach-session", "-t", "test"],
    ] {
        let failed = fixture.run_unchecked(&args, true);
        assert!(
            !failed.status.success(),
            "SSH client unexpectedly started replacement server: {failed:?}"
        );
        // C may leave a stale socket pathname after exit. No live replacement
        // may appear, and Linux additionally checks the pinned socket inode.
        #[cfg(unix)]
        assert!(std::os::unix::net::UnixStream::connect(&fixture.socket).is_err());
        #[cfg(target_os = "linux")]
        assert!(!fixture.owner.as_ref().unwrap().endpoint_replaced().unwrap());
        #[cfg(windows)]
        assert!(!fixture.reference.server_alive());
    }
}
