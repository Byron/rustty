#![cfg(unix)]
use std::{
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from("/tmp").join(format!(
            "tx-sock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn command(&self, program: &std::path::Path, args: &[&str], inherited: Option<&str>) -> Output {
        let mut cmd = Command::new(program);
        cmd.env_remove("TMUX")
            .env_remove("TMUXX_ENDPOINT")
            .env("TMUX_TMPDIR", &self.0)
            .args(["-f", "/dev/null"])
            .args(args);
        if let Some(inherited) = inherited {
            cmd.env("TMUX", inherited);
        }
        cmd.output().unwrap()
    }
    fn rust(&self, args: &[&str], inherited: Option<&str>) -> Output {
        self.command(env!("CARGO_BIN_EXE_tmuxx").as_ref(), args, inherited)
    }
    fn socket(&self, label: &str) -> PathBuf {
        self.0
            .canonicalize()
            .unwrap()
            .join(format!("tmux-{}", unsafe { libc::getuid() }))
            .join(label)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for label in ["default", "named", "cserver"] {
            let _ = self.rust(&["-L", label, "kill-server"], None);
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn good(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
#[test]
fn default_and_label_sockets_use_tmux_namespace_with_separate_native_endpoint() {
    let fixture = Fixture::new();
    good(fixture.rust(&["new-session", "-d", "-s", "default", "sleep", "30"], None));
    assert!(fixture.socket("default").exists());
    assert!(fixture.socket("default.native").exists());
    let server_pid = String::from_utf8(good(
        fixture.rust(&["display-message", "-p", "#{pid}"], None),
    ))
    .unwrap()
    .trim()
    .parse::<libc::pid_t>()
    .unwrap();
    assert!(server_pid > 0);
    assert_eq!(unsafe { libc::getsid(server_pid) }, server_pid);
    assert_eq!(unsafe { libc::getpgid(server_pid) }, server_pid);
    good(fixture.rust(
        &[
            "-L",
            "named",
            "new-session",
            "-d",
            "-s",
            "named",
            "sleep",
            "30",
        ],
        None,
    ));
    let inherited = format!("{},123,0", fixture.socket("named").display());
    assert_eq!(
        good(fixture.rust(
            &["list-sessions", "-F", "#{session_name}"],
            Some(&inherited)
        )),
        b"named\n"
    );
    assert_eq!(
        good(fixture.rust(
            &["-L", "default", "list-sessions", "-F", "#{session_name}"],
            Some(&inherited)
        )),
        b"default\n"
    );
    assert_eq!(
        good(fixture.rust(
            &[
                "-S",
                fixture.socket("default").to_str().unwrap(),
                "-L",
                "named",
                "list-sessions",
                "-F",
                "#{session_name}"
            ],
            Some(&inherited)
        )),
        b"default\n"
    );
}

#[test]
fn startup_uses_all_validated_command_metadata_and_never_starts_invalid_groups() {
    let fixture = Fixture::new();
    let invalid = fixture.rust(&["new-session", "-d", ";", "unknown-command"], None);
    assert!(!invalid.status.success());
    assert!(!fixture.socket("default").exists());
    assert_eq!(
        good(fixture.rust(&["list-comm", "-F", "#{command_list_name}", "new"], None)),
        b"new-session\n"
    );
    assert!(fixture.socket("default").exists());
}
#[test]
#[ignore = "requires separately selected C tmux executable in TMUXX_C_TMUX"]
fn unmodified_c_and_rust_clients_share_default_and_label_socket_resolution() {
    let fixture = Fixture::new();
    let c = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
    good(fixture.rust(&["new-session", "-d", "-s", "rust", "sleep", "30"], None));
    assert_eq!(
        good(fixture.command(&c, &["list-sessions", "-F", "#{session_name}"], None)),
        b"rust\n"
    );
    good(fixture.command(
        &c,
        &[
            "-L",
            "cserver",
            "new-session",
            "-d",
            "-s",
            "c",
            "sleep",
            "30",
        ],
        None,
    ));
    assert_eq!(
        good(fixture.rust(
            &["-L", "cserver", "list-sessions", "-F", "#{session_name}"],
            None
        )),
        b"c\n"
    );
    let inherited = format!("{},123,0", fixture.socket("cserver").display());
    assert_eq!(
        good(fixture.rust(
            &["list-sessions", "-F", "#{session_name}"],
            Some(&inherited)
        )),
        b"c\n"
    );
    assert_eq!(
        good(fixture.command(
            &c,
            &["-L", "default", "list-sessions", "-F", "#{session_name}"],
            Some(&inherited)
        )),
        b"rust\n"
    );
}

#[test]
fn explicit_native_control_wait_exit_observes_client_flags_and_newline() {
    use std::{
        io::{BufRead, BufReader, Write},
        process::Stdio,
        time::{Duration, Instant},
    };
    let fixture = Fixture::new();
    good(fixture.rust(&["new-session", "-d", "-s", "main", "sleep", "30"], None));
    let mut child = Command::new(env!("CARGO_BIN_EXE_tmuxx"))
        .arg("--native-endpoint")
        .arg(fixture.socket("default.native"))
        .args(["-C", "attach-session", "-t", "main"])
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let wait = |expected: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let line = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("missing native control record");
            if line.contains(expected) {
                break;
            }
        }
    };
    wait("%end ");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"refresh-client -f wait-exit\n")
        .unwrap();
    wait("%end ");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"detach-client\n")
        .unwrap();
    wait("%exit");
    std::thread::sleep(Duration::from_millis(40));
    assert!(
        child.try_wait().unwrap().is_none(),
        "native client ignored wait-exit"
    );
    child.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("native client failed to finish wait-exit");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(child.wait_with_output().unwrap().status.success());
    reader.join().unwrap();
}

#[test]
fn native_control_rejects_old_servers_before_submitting_commands() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        os::unix::net::UnixListener,
        time::Duration,
    };
    let fixture = Fixture::new();
    let endpoint = fixture.0.join("old.native");
    let listener = UnixListener::bind(&endpoint).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut hello = String::new();
        BufReader::new(&stream).read_line(&mut hello).unwrap();
        assert!(
            serde_json::from_str::<tmuxx_protocol::native::Hello>(&hello)
                .unwrap()
                .control_commands
        );
        stream.write_all(b"{\"version\":1}\n").unwrap();
        assert_eq!(
            stream.read(&mut [0; 1]).unwrap(),
            0,
            "sent a command without control capability"
        );
    });
    let result = fixture.rust(
        &[
            "--native-endpoint",
            endpoint.to_str().unwrap(),
            "-C",
            "new-session",
            "-s",
            "must-not-execute",
        ],
        None,
    );
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("native server does not support control command framing")
    );
    server.join().unwrap();
}

#[test]
fn clap_help_global_errors_and_excluded_modes_leave_no_server() {
    let fixture = Fixture::new();
    for flag in ["-h", "--help"] {
        let output = fixture.rust(&[flag], None);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("Headless tmux-compatible")
        );
    }
    for args in [
        vec!["-S"],
        vec!["--no-such-flag"],
        vec!["-v", "new-session", "-d"],
        vec!["-l", "new-session", "-d"],
        vec!["-c", "echo never"],
        vec!["-D", "new-session"],
    ] {
        let output = fixture.rust(&args, None);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
        assert!(
            !fixture.socket("default").exists(),
            "{args:?} started a server"
        );
    }
    assert_eq!(
        good(fixture.rust(&["-V"], None)),
        format!("tmuxx {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
}

#[test]
fn startup_failure_reports_cause_and_preserves_existing_path() {
    let fixture = Fixture::new();
    // Create the private socket directory before placing an unrelated file.
    assert!(
        !fixture
            .rust(&["-N", "list-sessions"], None)
            .status
            .success()
    );
    let endpoint = fixture.socket("default.native");
    std::fs::write(&endpoint, b"preserved").unwrap();
    let output = fixture.rust(&["new-session", "-d"], None);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("server startup failed:"), "{error}");
    assert!(error.contains("Address already in use"), "{error}");
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(endpoint).unwrap(), b"preserved");
    assert!(!fixture.socket("default").exists());
}

#[test]
fn startup_waits_for_a_closing_owner_lock_and_converges_concurrent_clients() {
    use std::{
        fs::OpenOptions,
        process::Stdio,
        time::{Duration, Instant},
    };
    let fixture = Fixture::new();
    assert!(
        !fixture
            .rust(&["-N", "list-sessions"], None)
            .status
            .success()
    );
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.socket("default.native.lock"))
        .unwrap();
    lock.lock().unwrap();
    let mut children = (0..8)
        .map(|index| {
            Command::new(env!("CARGO_BIN_EXE_tmuxx"))
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .env("TMUX_TMPDIR", &fixture.0)
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    &format!("client{index}"),
                    "sleep 30",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    std::thread::sleep(Duration::from_millis(100));
    for child in &mut children {
        assert!(
            child.try_wait().unwrap().is_none(),
            "client failed before owner released its lock"
        );
    }
    drop(lock);
    let deadline = Instant::now() + Duration::from_secs(8);
    for mut child in children {
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("startup did not finish");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        good(child.wait_with_output().unwrap());
    }
    let sessions = good(fixture.rust(&["list-sessions", "-F", "#{session_name}"], None));
    assert_eq!(
        sessions,
        (0..8)
            .map(|index| format!("client{index}\n"))
            .collect::<String>()
            .as_bytes()
    );
    let identity = good(fixture.rust(&["display", "-p", "#{pid}"], None));
    assert_eq!(
        good(fixture.rust(&["start", ";", "display", "-p", "#{pid}"], None)),
        identity
    );
    assert_eq!(
        good(fixture.rust(
            &[
                "--native-endpoint",
                fixture.socket("default.native").to_str().unwrap(),
                "start-server",
                ";",
                "display",
                "-p",
                "#{pid}"
            ],
            None
        )),
        identity
    );
}
