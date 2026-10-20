#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Hello, Request, Response, ServerMessage, SnapshotAssembler};

struct Owner {
    child: Child,
    directory: PathBuf,
    endpoint: PathBuf,
}
impl Owner {
    fn start() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "tx-life-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let endpoint = directory.join("native");
        let child = Self::spawn(&endpoint);
        Self {
            child,
            directory,
            endpoint,
        }
    }
    fn spawn(endpoint: &Path) -> Child {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tmuxx"))
            .args(["-D", "-f", "/dev/null", "--native-endpoint"])
            .arg(endpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        wait_until(|| {
            assert!(
                child.try_wait().unwrap().is_none(),
                "owner exited during startup"
            );
            UnixStream::connect(endpoint).is_ok()
        });
        child
    }
    fn restart(&mut self) {
        self.kill();
        self.child = Self::spawn(&self.endpoint);
    }
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(mut stream) = UnixStream::connect(&self.endpoint) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                if tmuxx_runtime::native_handshake(&mut stream).is_ok() {
                    let _ = serde_json::to_writer(
                        &mut stream,
                        &Request {
                            request_id: 1,
                            argv: vec![b"kill-server".to_vec()],
                            stdin: None,
                            subscribe: false,
                            control: false,
                        },
                    );
                    let _ = stream.write_all(b"\n");
                }
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.kill();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

// The view transport is a separately killable production native-stdio process.
struct View {
    child: Child,
    input: ChildStdin,
    messages: mpsc::Receiver<ServerMessage>,
    id: u64,
}
impl View {
    fn connect(endpoint: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tmuxx"))
            .args(["--native-stdio", "--native-endpoint"])
            .arg(endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        serde_json::to_writer(&mut input, &Hello::default()).unwrap();
        input.write_all(b"\n").unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let (hello_tx, hello_rx) = mpsc::channel();
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let Ok(hello) = serde_json::from_str::<Hello>(&line) else {
                return;
            };
            if hello_tx.send(hello).is_err() {
                return;
            }
            let mut assembler = SnapshotAssembler::default();
            for line in reader.lines() {
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
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        assert_eq!(
            hello_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .version,
            Hello::default().version
        );
        Self {
            child,
            input,
            messages,
            id: 0,
        }
    }
    fn command(&mut self, argv: &[&str], subscribe: bool) -> Response {
        self.id += 1;
        let request = Request {
            request_id: self.id,
            argv: argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            stdin: None,
            subscribe,
            control: false,
        };
        serde_json::to_writer(&mut self.input, &request).unwrap();
        self.input.write_all(b"\n").unwrap();
        loop {
            if let ServerMessage::Reply(reply) = self.recv()
                && reply.request_id == self.id
            {
                assert_eq!(reply.error, None, "{argv:?}");
                return reply;
            }
        }
    }
    fn recv(&self) -> ServerMessage {
        self.messages
            .recv_timeout(Duration::from_secs(5))
            .expect("native response timeout")
    }
    fn snapshot(&mut self, session: &str) -> (String, serde_json::Value) {
        self.command(&["attach-session", "-t", session], true);
        loop {
            if let ServerMessage::Snapshot {
                instance_id, state, ..
            } = self.recv()
            {
                return (instance_id, state);
            }
        }
    }
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for View {
    fn drop(&mut self) {
        self.kill();
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "lifetime condition did not converge"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn alive(pid: i32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    output.status.success() && !output.stdout.is_empty() && !output.stdout.starts_with(b"Z")
}
struct ProcessCleanup(
    Vec<i32>,
    #[cfg(target_os = "linux")] Vec<std::os::fd::OwnedFd>,
);
impl ProcessCleanup {
    fn new(pids: Vec<i32>) -> Self {
        #[cfg(target_os = "linux")]
        let handles = {
            use std::os::fd::FromRawFd;
            pids.iter()
                .filter_map(|pid| {
                    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, *pid, 0) } as i32;
                    if fd < 0 {
                        assert_eq!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::ESRCH)
                        );
                        None
                    } else {
                        Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
                    }
                })
                .collect()
        };
        Self(
            pids,
            #[cfg(target_os = "linux")]
            handles,
        )
    }
}
impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        for handle in &self.1 {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    handle.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
        #[cfg(not(target_os = "linux"))]
        for pid in &self.0 {
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
    }
}

#[test]
fn abrupt_owner_death_closes_pty_and_foreground_descendants_without_harming_other_owner() {
    let mut owner = Owner::start();
    let independent = Owner::start();
    let mut view = View::connect(&owner.endpoint);
    let mut other = View::connect(&independent.endpoint);
    let pids = owner.directory.join("pids");
    let script = "import os,sys,time\nchild=os.fork()\nif child == 0: time.sleep(60)\nelse:\n open(sys.argv[1],'w').write(str(os.getpid())+' '+str(child))\n time.sleep(60)";
    view.command(
        &[
            "new-session",
            "-d",
            "-s",
            "owned",
            "python3",
            "-u",
            "-c",
            script,
            pids.to_str().unwrap(),
        ],
        false,
    );
    other.command(
        &["new-session", "-d", "-s", "independent", "sleep", "60"],
        false,
    );
    let independent_pid = String::from_utf8(
        other
            .command(&["display-message", "-p", "#{pane_pid}"], false)
            .output,
    )
    .unwrap()
    .trim()
    .parse::<i32>()
    .unwrap();
    wait_until(|| std::fs::read_to_string(&pids).is_ok_and(|s| s.split_whitespace().count() == 2));
    let cleanup = ProcessCleanup::new(
        std::fs::read_to_string(pids)
            .unwrap()
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect(),
    );
    assert!(cleanup.0.iter().all(|pid| alive(*pid)));
    owner.kill();
    wait_until(|| cleanup.0.iter().all(|pid| !alive(*pid)));
    assert!(UnixStream::connect(&owner.endpoint).is_err());
    assert!(alive(independent_pid));
    assert_eq!(
        other
            .command(&["display-message", "-p", "#{pane_pid}"], false)
            .output,
        format!("{independent_pid}\n").as_bytes()
    );
}

#[test]
fn killed_client_relaunch_retains_selected_linked_pane_and_detached_queries_run_once() {
    let owner = Owner::start();
    let mut view = View::connect(&owner.endpoint);
    let trigger = owner.directory.join("trigger");
    let reply = owner.directory.join("reply");
    let script = "import os,sys,time,tty,select\ntty.setraw(0)\nos.write(1,b'READY')\nwhile not os.path.exists(sys.argv[1]): time.sleep(.005)\nos.write(1,b'\\r\\nDETACHED\\x1b[6n')\ndata=b''\nwhile not data.endswith(b'R'): data+=os.read(0,1)\nif select.select([0],[],[],.1)[0]: data+=os.read(0,4096)\nopen(sys.argv[2],'wb').write(data)\ntime.sleep(60)";
    view.command(
        &["new-session", "-d", "-s", "original", "sleep", "60"],
        true,
    );
    view.command(
        &[
            "split-window",
            "-h",
            "-t",
            "%0",
            "python3",
            "-u",
            "-c",
            script,
            trigger.to_str().unwrap(),
            reply.to_str().unwrap(),
        ],
        false,
    );
    view.command(&["tmuxx-retain-pane", "-t", "%0", "off"], false);
    view.command(&["tmuxx-retain-pane", "-t", "%1", "on"], false);
    view.command(&["new-session", "-d", "-s", "linked", "sleep", "60"], false);
    view.command(
        &["link-window", "-s", "original:0", "-t", "linked:5"],
        false,
    );
    let pid = view
        .command(&["display-message", "-p", "-t", "%1", "#{pane_pid}"], false)
        .output;
    wait_until(|| {
        view.command(&["capture-pane", "-p", "-t", "%1"], false)
            .output
            .starts_with(b"READY")
    });
    view.kill();
    // No native client is alive while the owner services the application query.
    std::fs::write(&trigger, b"go").unwrap();
    wait_until(|| std::fs::read(&reply).is_ok_and(|bytes| !bytes.is_empty()));
    assert_eq!(std::fs::read(reply).unwrap(), b"\x1b[2;9R");
    let mut relaunched = View::connect(&owner.endpoint);
    wait_until(|| {
        let output = relaunched
            .command(&["list-panes", "-a", "-F", "#{pane_id}"], false)
            .output;
        let mut panes = output
            .split(|b| *b == b'\n')
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>();
        panes.sort();
        panes == [b"%1", b"%1", b"%2"]
    });
    assert_eq!(
        relaunched
            .command(
                &["display-message", "-p", "-t", "linked:5", "#{pane_pid}"],
                false
            )
            .output,
        pid
    );
    let links = relaunched
        .command(
            &[
                "list-windows",
                "-a",
                "-F",
                "#{session_name}:#{window_index}:#{window_id}",
            ],
            false,
        )
        .output;
    let mut links = links
        .split(|b| *b == b'\n')
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>();
    links.sort();
    assert_eq!(
        links,
        [b"linked:0:@1".as_slice(), b"linked:5:@0", b"original:0:@0"]
    );
    let (_, state) = relaunched.snapshot("linked");
    assert!(
        state["panes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pane| pane["id"] == 1)
    );
    assert!(
        String::from_utf8_lossy(
            &relaunched
                .command(&["capture-pane", "-p", "-t", "%1"], false)
                .output
        )
        .contains("DETACHED")
    );
    assert_eq!(
        relaunched
            .command(&["display-message", "-p", "-t", "%1", "#{pane_pid}"], false)
            .output,
        pid
    );
}

#[test]
fn reused_endpoint_and_pane_id_have_a_new_server_incarnation() {
    let mut owner = Owner::start();
    let mut before = View::connect(&owner.endpoint);
    before.command(&["new-session", "-d", "-s", "before", "sleep", "60"], false);
    let (instance_before, state_before) = before.snapshot("before");
    owner.restart();
    let mut after = View::connect(&owner.endpoint);
    after.command(&["new-session", "-d", "-s", "after", "sleep", "60"], false);
    let (instance_after, state_after) = after.snapshot("after");
    assert_ne!(instance_before, instance_after);
    assert_eq!(state_before["panes"][0]["id"], 0);
    assert_eq!(state_after["panes"][0]["id"], 0);
    assert_ne!(
        state_before["panes"][0]["pid"],
        state_after["panes"][0]["pid"]
    );
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn killed_owner_watchdog_cleans_hup_ignoring_session_jobs_and_preserves_independent_sessions() {
    for leader_exits in [false, true] {
        let mut owner = Owner::start();
        let independent = Owner::start();
        let mut view = View::connect(&owner.endpoint);
        let mut other = View::connect(&independent.endpoint);
        let path = owner.directory.join("ignoring-pids");
        let exit = owner.directory.join("exit-leader");
        let script = "import os,signal,sys,time\nsignal.signal(signal.SIGHUP,signal.SIG_IGN)\nfg=os.fork()\nif fg == 0: time.sleep(60); os._exit(0)\nbg=os.fork()\nif bg == 0: os.setpgid(0,0); time.sleep(60); os._exit(0)\nescaped=os.fork()\nif escaped == 0: os.setsid(); time.sleep(60); os._exit(0)\nwhile os.getpgid(bg)!=bg or os.getsid(escaped)!=escaped: time.sleep(.002)\nopen(sys.argv[1],'w').write(' '.join(map(str,[os.getpid(),fg,bg,escaped])))\nwhile not os.path.exists(sys.argv[2]): time.sleep(.002)\nos._exit(0)";
        view.command(
            &[
                "new-session",
                "-d",
                "-s",
                "owned",
                "python3",
                "-u",
                "-c",
                script,
                path.to_str().unwrap(),
                exit.to_str().unwrap(),
            ],
            false,
        );
        other.command(
            &["new-session", "-d", "-s", "independent", "sleep", "60"],
            false,
        );
        let independent_pid = String::from_utf8(
            other
                .command(&["display-message", "-p", "#{pane_pid}"], false)
                .output,
        )
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
        wait_until(|| {
            std::fs::read_to_string(&path).is_ok_and(|text| text.split_whitespace().count() == 4)
        });
        let cleanup = ProcessCleanup::new(
            std::fs::read_to_string(path)
                .unwrap()
                .split_whitespace()
                .map(|pid| pid.parse().unwrap())
                .collect(),
        );
        // Prevent normal EOF/reaper cleanup from racing the watchdog-only case.
        assert_eq!(
            unsafe { libc::kill(owner.child.id() as i32, libc::SIGSTOP) },
            0
        );
        let mut stopped = 0;
        assert_eq!(
            unsafe { libc::waitpid(owner.child.id() as i32, &mut stopped, libc::WUNTRACED) },
            owner.child.id() as i32
        );
        assert!(libc::WIFSTOPPED(stopped));
        if leader_exits {
            std::fs::write(exit, b"exit").unwrap();
            wait_until(|| !alive(cleanup.0[0]));
        }
        assert!(cleanup.0[1..].iter().all(|pid| alive(*pid)));
        assert_eq!(unsafe { libc::getsid(cleanup.0[1]) }, cleanup.0[0]);
        assert_eq!(unsafe { libc::getsid(cleanup.0[2]) }, cleanup.0[0]);
        assert_eq!(unsafe { libc::getpgid(cleanup.0[2]) }, cleanup.0[2]);
        owner.kill();
        wait_until(|| cleanup.0[..3].iter().all(|pid| !alive(*pid)));
        assert!(
            alive(cleanup.0[3]),
            "a deliberately separate OS session is outside the owner's cleanup scope"
        );
        assert!(alive(independent_pid));
        assert!(
            UnixStream::connect(&owner.endpoint).is_err(),
            "watchdog must not retain the owner's listening descriptor"
        );
        assert_eq!(
            other
                .command(&["display-message", "-p", "#{pane_pid}"], false)
                .output,
            format!("{independent_pid}\n").as_bytes()
        );
    }
}
