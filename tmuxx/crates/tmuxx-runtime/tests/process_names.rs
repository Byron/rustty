#![cfg(unix)]
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tmuxx_protocol::native::{Request, ServerMessage};
use tmuxx_runtime::{LocalClient, Options};

struct Oracle {
    binary: PathBuf,
    socket: PathBuf,
}
impl Oracle {
    fn run(&self, argv: &[&str]) -> Vec<u8> {
        let result = Command::new(&self.binary)
            .args(["-S"])
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(argv)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{argv:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }
}
impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .args(["-S"])
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}
fn run(client: &LocalClient, argv: &[&str]) -> Vec<u8> {
    client
        .send(Request {
            request_id: 1,
            argv: argv.iter().map(|s| s.as_bytes().to_vec()).collect(),
            subscribe: false,
            control: false,
            stdin: None,
        })
        .unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{argv:?}");
            return reply.output;
        }
        assert!(Instant::now() < end, "{argv:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn wait(query: impl Fn() -> Vec<u8>, expected: &[u8]) {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let actual = query();
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < end,
            "expected {:?}, got {:?}",
            String::from_utf8_lossy(expected),
            String::from_utf8_lossy(&actual)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn foreground_path_and_automatic_names_follow_real_processes() {
    let directory = std::env::temp_dir().join(format!(
        "tx-names-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let shell = if cfg!(target_os = "macos") {
        "bash"
    } else {
        "sh"
    };
    let first = directory.join("first");
    let second = directory.join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let host = tmuxx_runtime::start(Options::new(directory.join("r.sock"))).unwrap();
    let client = host.connect().unwrap();
    let oracle = Oracle {
        binary: std::env::var_os("TMUXX_C_TMUX")
            .expect("set TMUXX_C_TMUX")
            .into(),
        socket: directory.join("c.sock"),
    };
    let both = |args: &[&str]| {
        let actual = run(&client, args);
        let expected = oracle.run(args);
        assert_eq!(actual, expected, "{args:?}");
    };
    // Some Linux distributions ship uutils tty, which may omit the newline.
    // Command substitution plus printf gives both oracles one explicit line.
    let script = format!(
        "printf '%s\\n' \"$(tty)\"; cd '{}'; printf READY; read step; cd '{}'; printf SECOND; read step; exec /bin/sleep 30",
        first.display(),
        second.display()
    );
    both(&[
        "new", "-d", "-s", "main", "-n", "pinned", "-c", "/", "/bin/sh", "-c", &script,
    ]);
    let format = "#{pane_current_command}|#{pane_current_path}|#{window_name}";
    let args = ["display", "-pt", "%0", format];
    let expected = format!("{shell}|{}|pinned\n", first.display());
    wait(|| run(&client, &args), expected.as_bytes());
    wait(|| oracle.run(&args), expected.as_bytes());
    let path = ["display", "-pt", "%0", "#{pane_start_path}"];
    assert_eq!(run(&client, &path), b"/\n");
    assert_eq!(oracle.run(&path), b"/\n");
    let tty = ["display", "-pt", "%0", "#{pane_tty}"];
    let capture = ["capture-pane", "-pt", "%0", "-S", "-", "-E", "-"];
    for (implementation, tty, capture) in [
        ("Rust", run(&client, &tty), run(&client, &capture)),
        ("C", oracle.run(&tty), oracle.run(&capture)),
    ] {
        assert!(tty.starts_with(b"/dev/"), "{tty:?}");
        assert_eq!(
            tty.strip_suffix(b"\n"),
            capture.split(|b| *b == b'\n').next(),
            "{implementation} PTY first line: {capture:?}"
        );
    }
    both(&[
        "set",
        "-w",
        "automatic-rename-format",
        "run=#{pane_current_command}",
    ]);
    both(&["set", "-w", "automatic-rename", "on"]);
    let expected = format!("{shell}|{}|run={shell}\n", first.display());
    wait(|| run(&client, &args), expected.as_bytes());
    wait(|| oracle.run(&args), expected.as_bytes());
    both(&[
        "set-hook",
        "-g",
        "window-renamed",
        "set -g @renamed '#{hook_window_name}'",
    ]);
    both(&["send", "-t", "%0", "go", "Enter"]);
    let expected = format!("{shell}|{}|run={shell}\n", second.display());
    wait(|| run(&client, &args), expected.as_bytes());
    wait(|| oracle.run(&args), expected.as_bytes());
    both(&["send", "-t", "%0", "go", "Enter"]);
    let expected = format!("sleep|{}|run=sleep\n", second.display());
    wait(|| run(&client, &args), expected.as_bytes());
    wait(|| oracle.run(&args), expected.as_bytes());
    assert_eq!(run(&client, &path), b"/\n");
    assert_eq!(oracle.run(&path), b"/\n");
    both(&["show", "-gv", "@renamed"]);
    both(&["renamew", "-t", "main:0", "fixed"]);
    both(&["showw", "-v", "automatic-rename"]);
    both(&["set", "-w", "automatic-rename-format", "ignored"]);
    std::thread::sleep(Duration::from_millis(600));
    both(&["display", "-pt", "%0", "#{window_name}"]);
    drop(oracle);
    drop(client);
    drop(host);
    std::fs::remove_dir_all(directory).unwrap();
}
