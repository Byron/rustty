#![cfg(target_os = "linux")]
//! Real OpenSSH exec channels to an existing Linux native owner. No remote PTY.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Hello, Request, Response, ServerMessage, SnapshotAssembler};

const LIMIT: Duration = Duration::from_secs(8);

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + LIMIT;
    while !done() {
        assert!(Instant::now() < deadline, "native SSH fixture timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_child(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("owned child {} did not exit before deadline", child.id());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn quote(value: &Path) -> String {
    format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
}

// A pidfd pins the acknowledged remote exec process; cleanup never signals a
// numeric PID which might have been reused while a test was unwinding.
struct PinnedProcess(OwnedFd);
impl PinnedProcess {
    fn open(pid: u32) -> Self {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(fd >= 0, "pidfd_open: {}", std::io::Error::last_os_error());
        Self(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }
    fn exited(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut poll, 1, 0) > 0 }
    }
    fn kill(&self) {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
    }
}
impl Drop for PinnedProcess {
    fn drop(&mut self) {
        if !self.exited() {
            self.kill();
        }
    }
}

struct RunningChild(Child);
impl Deref for RunningChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl DerefMut for RunningChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for RunningChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    directory: PathBuf,
    executable: PathBuf,
    endpoint: PathBuf,
    user: String,
    port: u16,
    sshd: Option<RunningChild>,
    owner: Option<RunningChild>,
}
impl Fixture {
    fn start() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "tx-native-ssh-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        let mut fixture = Self {
            executable: directory.join("tmuxx ' ; $literal"),
            endpoint: directory.join("native ' ; $literal"),
            directory,
            user,
            port,
            sshd: None,
            owner: None,
        };
        symlink(env!("CARGO_BIN_EXE_tmuxx"), &fixture.executable).unwrap();
        for name in ["host", "client"] {
            let output = Command::new("/usr/bin/ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(fixture.directory.join(name))
                .output()
                .unwrap();
            assert!(output.status.success(), "ssh-keygen: {output:?}");
        }
        let public = fs::read_to_string(fixture.directory.join("host.pub")).unwrap();
        let fields = public.split_whitespace().collect::<Vec<_>>();
        fs::write(
            fixture.directory.join("known_hosts"),
            format!("[127.0.0.1]:{port} {} {}\n", fields[0], fields[1]),
        )
        .unwrap();
        fs::write(fixture.directory.join("sshd.conf"), format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {dir}/host\nPidFile {dir}/pid\nAuthorizedKeysFile {dir}/client.pub\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nAllowUsers {user}\nLogLevel ERROR\nPermitTTY no\nAllowTcpForwarding no\nX11Forwarding no\n",
            dir=fixture.directory.display(), user=fixture.user)).unwrap();
        fixture.sshd = Some(RunningChild(
            Command::new("/usr/sbin/sshd")
                .args(["-D", "-e", "-f"])
                .arg(fixture.directory.join("sshd.conf"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(fixture.directory.join("sshd.log")).unwrap())
                .spawn()
                .expect("OpenSSH server must be installed"),
        ));
        wait_until(|| {
            assert!(
                fixture.sshd.as_mut().unwrap().try_wait().unwrap().is_none(),
                "sshd exited: {}",
                fs::read_to_string(fixture.directory.join("sshd.log")).unwrap()
            );
            TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        fixture.owner = Some(RunningChild(
            Command::new(&fixture.executable)
                .args(["-D", "-f", "/dev/null", "--native-endpoint"])
                .arg(&fixture.endpoint)
                .env_remove("TMUX")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(fixture.directory.join("owner.log")).unwrap())
                .spawn()
                .unwrap(),
        ));
        wait_until(|| {
            assert!(
                fixture
                    .owner
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none(),
                "owner exited: {}",
                fs::read_to_string(fixture.directory.join("owner.log")).unwrap()
            );
            UnixStream::connect(&fixture.endpoint).is_ok()
        });
        fixture
    }
    fn ssh(&self, endpoint: &Path) -> Command {
        // The shell PID survives exec, letting the client acknowledge exactly the
        // process it may clean up. Both tty checks must pass before the bridge runs.
        let command = format!(
            "test ! -t 0 && test ! -t 1 || exit 90; printf 'TMUXX_BRIDGE_PID=%s\\n' \"$$\" >&2; exec {} --native-stdio --native-endpoint {}",
            quote(&self.executable),
            quote(endpoint)
        );
        let mut ssh = Command::new("/usr/bin/ssh");
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
                "ConnectionAttempts=1",
                "-o",
                "ControlMaster=no",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
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
    fn direct(&self, argv: &[&str]) -> Response {
        let mut stream = UnixStream::connect(&self.endpoint).unwrap();
        stream.set_read_timeout(Some(LIMIT)).unwrap();
        stream.set_write_timeout(Some(LIMIT)).unwrap();
        tmuxx_runtime::native_handshake(&mut stream).unwrap();
        let request = Request {
            request_id: 1,
            argv: argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            stdin: None,
            subscribe: false,
            control: false,
        };
        serde_json::to_writer(&mut stream, &request).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if let ServerMessage::Reply(reply) = serde_json::from_str(&line).unwrap() {
                assert_eq!(reply.error, None, "{argv:?}");
                return reply;
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.as_mut() {
            if owner.try_wait().ok().flatten().is_none()
                && let Ok(mut stream) = UnixStream::connect(&self.endpoint)
            {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                if tmuxx_runtime::native_handshake(&mut stream).is_ok() {
                    let _ = stream.write_all(b"{\"request_id\":1,\"argv\":[[107,105,108,108,45,115,101,114,118,101,114]]}\n");
                }
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while owner.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = owner.kill();
            let _ = owner.wait();
        }
        if let Some(sshd) = self.sshd.as_mut() {
            let _ = sshd.kill();
            let _ = sshd.wait();
        }
        if std::thread::panicking() {
            eprintln!(
                "native SSH failure transcripts retained in {}",
                self.directory.display()
            );
        } else {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

struct Remote {
    child: RunningChild,
    input: Option<ChildStdin>,
    messages: Receiver<ServerMessage>,
    process: PinnedProcess,
    id: u64,
}
impl Remote {
    fn connect(fixture: &Fixture) -> Self {
        static NEXT_LOG: AtomicU64 = AtomicU64::new(0);
        let log = fs::File::create(fixture.directory.join(format!(
            "bridge-{}.log",
            NEXT_LOG.fetch_add(1, Ordering::Relaxed)
        )))
        .unwrap();
        let mut child = RunningChild(
            fixture
                .ssh(&fixture.endpoint)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut input = child.stdin.take().unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let (pid_tx, pid_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut log = log;
            let mut first = String::new();
            if stderr.read_line(&mut first).is_ok() {
                let _ = log.write_all(first.as_bytes());
                let _ = pid_tx.send(first);
            }
            let _ = std::io::copy(&mut stderr, &mut log);
        });
        let marker = pid_rx
            .recv_timeout(LIMIT)
            .expect("SSH exec did not acknowledge PID");
        let pid = marker
            .trim()
            .strip_prefix("TMUXX_BRIDGE_PID=")
            .expect("unexpected SSH stderr before exec")
            .parse::<u32>()
            .unwrap();
        let process = PinnedProcess::open(pid);
        let mut output = BufReader::new(child.stdout.take().unwrap());
        serde_json::to_writer(&mut input, &Hello::default()).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
        let (hello_tx, hello_rx) = mpsc::channel();
        // Bounded delivery deliberately backpressures SSH when the application
        // stops consuming replies; it cannot hide slow clients in an unbounded Vec.
        let (tx, messages) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = String::new();
            if output.read_line(&mut line).is_err() {
                return;
            }
            let hello = serde_json::from_str::<Hello>(&line);
            if hello_tx.send(hello).is_err() {
                return;
            }
            let mut assembler = SnapshotAssembler::default();
            for line in output.lines() {
                let Ok(line) = line else {
                    break;
                };
                let Ok(message) = serde_json::from_str(&line) else {
                    break;
                };
                match assembler.push(message) {
                    Ok(Some(message)) => {
                        if tx.send(message).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
        assert_eq!(
            hello_rx
                .recv_timeout(LIMIT)
                .expect("native SSH hello timed out")
                .expect("stdout was not a native Hello")
                .version,
            Hello::default().version
        );
        assert_eq!(
            fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
            fs::canonicalize(env!("CARGO_BIN_EXE_tmuxx")).unwrap(),
            "SSH exec must run the intended bridge binary"
        );
        Self {
            child,
            input: Some(input),
            messages,
            process,
            id: 0,
        }
    }
    fn send(&mut self, argv: &[&str], stdin: Option<Vec<u8>>, subscribe: bool) -> u64 {
        self.id += 1;
        let request = Request {
            request_id: self.id,
            argv: argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            stdin,
            subscribe,
            control: false,
        };
        let input = self.input.as_mut().unwrap();
        let mut bytes = serde_json::to_vec(&request).unwrap();
        bytes.push(b'\n');
        // Fragment every command independently of OpenSSH packet boundaries.
        for chunk in bytes.chunks(31) {
            input.write_all(chunk).unwrap();
        }
        input.flush().unwrap();
        self.id
    }
    fn receive(&self) -> ServerMessage {
        self.messages
            .recv_timeout(LIMIT)
            .expect("native SSH reply timed out/disconnected")
    }
    fn reply(&self, id: u64) -> Response {
        loop {
            if let ServerMessage::Reply(reply) = self.receive()
                && reply.request_id == id
            {
                assert_eq!(reply.error, None, "native request {id}");
                return reply;
            }
        }
    }
    fn command(&mut self, argv: &[&str]) -> Vec<u8> {
        let id = self.send(argv, None, false);
        self.reply(id).output
    }
    fn close(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
        let _ = wait_child(&mut self.child);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.process.exited() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !self.process.exited() {
            self.process.kill();
        }
        wait_until(|| self.process.exited());
    }
}
impl Drop for Remote {
    fn drop(&mut self) {
        self.close();
    }
}

#[test]
#[ignore = "requires real OpenSSH server/client, Linux pidfds, sockets and PTYs"]
fn native_ssh_preserves_bytes_reconnects_and_never_replays_ambiguous_mutations() {
    let fixture = Fixture::start();
    let mut remote = Remote::connect(&fixture);
    remote.command(&[
        "new-session",
        "-d",
        "-s",
        "native",
        "python3",
        "-u",
        "-c",
        "import os,time; os.write(1,b'NATIVE-READY\\n'); time.sleep(60)",
    ]);
    remote.command(&["tmuxx-retain-pane", "-t", "%0", "on"]);
    assert_eq!(
        remote.command(&["display-message", "-p", "#{pid}"]),
        format!("{}\n", fixture.owner.as_ref().unwrap().id()).as_bytes()
    );
    let pid = remote.command(&["display-message", "-p", "#{pane_pid}"]);
    let pane = PinnedProcess::open(
        String::from_utf8(pid.clone())
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    );
    let quoted = "quotes ' \" ; $() `literal` \\ and newline\nend";
    remote.command(&["set-option", "-g", "@ssh", quoted]);
    assert_eq!(
        remote.command(&["show-option", "-gqv", "@ssh"]),
        format!("{quoted}\n").as_bytes()
    );
    let binary = (0..=255).cycle().take(65_536).collect::<Vec<u8>>();
    let id = remote.send(
        &["load-buffer", "-b", "ssh-binary", "-"],
        Some(binary.clone()),
        false,
    );
    remote.reply(id);
    assert_eq!(
        remote.command(&["save-buffer", "-b", "ssh-binary", "-"]),
        binary
    );
    let ids = (0..12)
        .map(|_| remote.send(&["save-buffer", "-b", "ssh-binary", "-"], None, false))
        .collect::<Vec<_>>();
    // Stop consuming this channel until a separate client demonstrates owner progress.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        fixture
            .direct(&["display-message", "-p", "owner-responsive"])
            .output,
        b"owner-responsive\n"
    );
    for id in ids {
        assert_eq!(remote.reply(id).output, binary);
    }
    remote.command(&["display-message", "-p", "pre-mutation-barrier"]);
    let ambiguous = remote.send(
        &["new-window", "-d", "-n", "ambiguous", "sleep", "60"],
        None,
        false,
    );
    assert!(ambiguous > 0);
    wait_until(|| {
        fixture
            .direct(&["list-windows", "-t", "native", "-F", "#{window_name}"])
            .output
            .split(|b| *b == b'\n')
            .any(|line| line == b"ambiguous")
    });
    // The mutation is observed independently but its reply is deliberately never
    // consumed. Native request IDs are connection local; reconnect sends no replay.
    remote.close();
    let mut reconnected = Remote::connect(&fixture);
    assert_eq!(
        reconnected.command(&["display-message", "-p", "-t", "%0", "#{pane_pid}"]),
        pid
    );
    assert_eq!(
        reconnected
            .command(&["list-windows", "-t", "native", "-F", "#{window_name}"])
            .split(|b| *b == b'\n')
            .filter(|line| *line == b"ambiguous")
            .count(),
        1
    );
    assert!(
        reconnected
            .command(&["capture-pane", "-p", "-t", "%0"])
            .windows(b"NATIVE-READY".len())
            .any(|bytes| bytes == b"NATIVE-READY")
    );
    let id = reconnected.send(&["attach-session", "-t", "native"], None, true);
    reconnected.reply(id);
    loop {
        if let ServerMessage::Snapshot { state, .. } = reconnected.receive() {
            assert!(!state.is_null());
            break;
        }
    }
    reconnected.close();
    assert!(
        !pane.exited(),
        "retained original pane must outlive SSH exec channels"
    );
    fixture.direct(&["kill-server"]);
    wait_until(|| pane.exited());
}

#[test]
#[ignore = "requires real OpenSSH server/client, Linux pidfds, sockets and PTYs"]
fn native_ssh_missing_owner_fails_without_creating_endpoint() {
    let fixture = Fixture::start();
    let missing = fixture.directory.join("missing ' ; $owner");
    let mut child = RunningChild(
        fixture
            .ssh(&missing)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let status = wait_child(&mut child);
    let mut stdout = Vec::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!status.success(), "missing native owner must fail");
    assert!(
        stdout.is_empty(),
        "diagnostics must stay outside the native protocol"
    );
    assert!(stderr.contains("TMUXX_BRIDGE_PID="), "{stderr}");
    assert!(
        stderr.contains("No such file") || stderr.contains("not found"),
        "{stderr}"
    );
    assert!(
        !missing.exists(),
        "bridge must never create a replacement owner"
    );
    assert_eq!(
        fixture
            .direct(&["display-message", "-p", "unrelated-owner-survives"])
            .output,
        b"unrelated-owner-survives\n"
    );
}
