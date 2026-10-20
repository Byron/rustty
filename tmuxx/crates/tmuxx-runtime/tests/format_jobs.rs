#![cfg(unix)]
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tmuxx_protocol::{
    Decoder, Event,
    native::{Request, ServerMessage},
};
use tmuxx_runtime::{ClientContext, LocalClient, Options, ServerHandle};

struct Oracle {
    child: Child,
    events: mpsc::Receiver<Event>,
    binary: PathBuf,
    socket: PathBuf,
}
impl Oracle {
    fn new(socket: &PathBuf, cwd: &PathBuf) -> Self {
        let binary = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
        let mut child = Command::new(&binary)
            .args(["-f", "/dev/null", "-S"])
            .arg(socket)
            .args([
                "-C",
                "new-session",
                "-s",
                "main",
                "-n",
                "fixed",
                "sleep 120",
            ])
            .current_dir(cwd)
            .env_remove("TMUX")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        std::thread::spawn(move || {
            let mut decoder = Decoder::default();
            let mut buffer = [0; 8192];
            loop {
                let n = stdout.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                for event in decoder.feed(&buffer[..n]).unwrap() {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
            }
        });
        let oracle = Self {
            child,
            events,
            binary,
            socket: socket.clone(),
        };
        assert!(oracle.reply().is_empty());
        oracle
    }
    fn reply(&self) -> Vec<u8> {
        let mut output = Vec::new();
        loop {
            match self
                .events
                .recv_timeout(Duration::from_secs(5))
                .expect("C control reply")
            {
                Event::CommandOutput(line) => {
                    output.extend(line);
                    output.push(b'\n');
                }
                Event::End { failed, .. } => {
                    assert!(
                        !failed,
                        "C command failed: {}",
                        String::from_utf8_lossy(&output)
                    );
                    return output;
                }
                _ => {}
            }
        }
    }
    fn run(&mut self, args: &[&str]) -> Vec<u8> {
        let command = tmuxx_command::Command {
            blocks: Default::default(),
            args: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
        };
        let bytes = tmuxx_command::serialize(&[command]);
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        self.child.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
        self.reply()
    }
}
impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
        let _ = self.child.wait();
    }
}
struct Pair {
    oracle: Oracle,
    client: LocalClient,
    _host: ServerHandle,
    dir: PathBuf,
}
impl Pair {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = PathBuf::from(format!(
            "/tmp/tx-fjob-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let host = tmuxx_runtime::start(Options::new(dir.join("r"))).unwrap();
        let client = host
            .connect_with_context(ClientContext {
                cwd: Some(dir.to_string_lossy().into()),
                ..ClientContext::default()
            })
            .unwrap();
        let mut pair = Self {
            oracle: Oracle::new(&dir.join("c"), &dir),
            client,
            _host: host,
            dir,
        };
        pair.rust(&["new-session", "-s", "main", "-n", "fixed", "sleep 120"]);
        pair.run(&["set-option", "-g", "default-shell", "/bin/sh"]);
        pair.oracle.run(&["set-environment", "-g", "SIDE", "c"]);
        pair.rust(&["set-environment", "-g", "SIDE", "r"]);
        pair
    }
    fn rust(&self, args: &[&str]) -> Vec<u8> {
        self.client
            .send(Request {
                request_id: 1,
                argv: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
                stdin: None,
                subscribe: false,
                control: true,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(ServerMessage::Reply(reply)) = self.client.try_recv() {
                assert_eq!(reply.error, None, "{args:?}");
                return reply.output;
            }
            assert!(Instant::now() < deadline, "Rust reply {args:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn run(&mut self, args: &[&str]) -> Vec<u8> {
        let c = self.oracle.run(args);
        let rust = self.rust(args);
        assert_eq!(rust, c, "{args:?}");
        rust
    }
    fn wait_format(&mut self, format: &str, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let c = self.oracle.run(&["display-message", "-p", format]);
            let rust = self.rust(&["display-message", "-p", format]);
            if c == expected && rust == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "format {format}: C {:?}, Rust {:?}, expected {:?}",
                String::from_utf8_lossy(&c),
                String::from_utf8_lossy(&rust),
                String::from_utf8_lossy(expected)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
#[ignore = "requires C tmux and socket/PTY access"]
fn asynchronous_format_jobs_stream_cache_expand_and_restart_like_c() {
    let mut pair = Pair::new();
    pair.run(&["set-option", "-g", "@value", "first"]);
    let dynamic = "#(printf '#{@value}')";
    assert_eq!(pair.run(&["display-message", "-p", dynamic]), b"\n");
    pair.wait_format(dynamic, b"first\n");
    pair.run(&["set-option", "-g", "@value", "changed"]);
    pair.wait_format(dynamic, b"changed\n");
    pair.wait_format("#(printf '\\043{\\100value}')", b"changed\n");
    let stream = "#(if [ -e done ]; then printf tail; exit; fi; printf 'first\\n'; while [ ! -e next ]; do sleep 0.01; done; printf 'second\\n'; while [ ! -e done ]; do sleep 0.01; done; printf tail)";
    assert_eq!(pair.run(&["display-message", "-p", stream]), b"\n");
    pair.wait_format(stream, b"first\n");
    std::fs::write(pair.dir.join("next"), b"").unwrap();
    pair.wait_format(stream, b"second\n");
    std::fs::write(pair.dir.join("done"), b"").unwrap();
    pair.wait_format(stream, b"tail\n");
    pair.wait_format("#(printf '\\043(touch forbidden)')", b"\n");
    pair.wait_format("#(printf 'out\\n'; printf error >&2; exit 7)", b"out\n");
    pair.wait_format("#(printf '#{session_name}:#(touch forbidden)')", b"main:\n");
    assert!(!pair.dir.join("forbidden").exists());
}

#[test]
#[ignore = "requires C tmux and socket/PTY access"]
fn repeated_format_jobs_share_live_work_and_report_not_ready() {
    let mut pair = Pair::new();
    let command =
        "printf x >> \"$SIDE.count\"; while [ ! -e release ]; do sleep 0.01; done; printf finished";
    let format = format!("#({command})");
    let twice = format!("{format}|{format}");
    assert_eq!(pair.run(&["display-message", "-p", &twice]), b"|\n");
    let expected = format!("<'{command}' not ready>\n");
    pair.wait_format(&format, expected.as_bytes());
    for side in ["c", "r"] {
        assert_eq!(
            std::fs::read(pair.dir.join(format!("{side}.count"))).unwrap(),
            b"x"
        );
    }
    std::fs::write(pair.dir.join("release"), b"").unwrap();
    pair.wait_format(&format, b"finished\n");
}

#[test]
#[ignore = "requires C tmux and socket/PTY access"]
fn format_jobs_stop_with_the_requesting_client() {
    let mut pair = Pair::new();
    let format = "#(echo $$ > \"$SIDE.pid\"; exec sleep 120)";
    assert_eq!(pair.run(&["display-message", "-p", format]), b"\n");
    let deadline = Instant::now() + Duration::from_secs(5);
    let paths = [pair.dir.join("c.pid"), pair.dir.join("r.pid")];
    while !paths.iter().all(|path| path.exists()) {
        assert!(Instant::now() < deadline, "format jobs did not write PIDs");
        std::thread::sleep(Duration::from_millis(2));
    }
    let pids = paths.map(|path| {
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap()
    });
    for pid in pids {
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    }
    pair.run(&["detach-client"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(|pid| unsafe { libc::kill(*pid, 0) } == 0) {
        assert!(
            Instant::now() < deadline,
            "format job survived its client: {pids:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
