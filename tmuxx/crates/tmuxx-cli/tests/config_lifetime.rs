#![cfg(unix)]

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Pending(Child);
impl Pending {
    fn finish(mut self) -> Output {
        wait(|| self.0.try_wait().unwrap().is_some());
        let status = self.0.wait().unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        self.0
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        self.0
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        Output {
            status,
            stdout,
            stderr,
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Server {
    program: PathBuf,
    directory: PathBuf,
}
impl Server {
    fn new(program: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(format!(
            "/tmp/tx-cfg-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self {
            program: program.into(),
            directory,
        }
    }
    fn spawn(&self, args: &[&str]) -> Pending {
        Pending(
            Command::new(&self.program)
                .args(["-S"])
                .arg(self.directory.join("socket"))
                .args(args)
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .env("SHELL", "/bin/sh")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::write(self.directory.join("go"), b"");
        let _ = self.spawn(&["-N", "kill-server"]).finish();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < deadline, "configuration lifetime timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn rejected_startup_clients_release_slots(socket: &Path) {
    use tmuxx_unix_ipc::msg;
    let mut bytes = Vec::new();
    for (kind, body) in [
        (msg::IDENTIFY_CWD, b"/tmp\0".to_vec()),
        (
            msg::IDENTIFY_CLIENTPID,
            std::process::id().to_ne_bytes().to_vec(),
        ),
        (msg::IDENTIFY_DONE, Vec::new()),
        (msg::COMMAND, Vec::new()),
    ] {
        for number in [kind, 16 + body.len() as u32, 8, std::process::id()] {
            bytes.extend(number.to_ne_bytes());
        }
        bytes.extend(body);
    }
    for _ in 0..140 {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(&bytes).unwrap();
        match stream.read(&mut [0; 1]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ) => {}
            result => panic!("malformed startup client was retained: {result:?}"),
        }
    }
}

#[test]
#[ignore = "requires separate C tmux and PTY/socket access"]
fn configuration_loads_once_after_starter_loss_and_waiters_observe_completion() {
    let c = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
    let mut results = Vec::new();
    for program in [c.as_path(), Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        let server = Server::new(program);
        let config = server.directory.join("conf");
        let ready = server.directory.join("ready");
        let background = server.directory.join("background");
        let go = server.directory.join("go");
        std::fs::write(&config, format!(
            "set -ga @runs x\nnew-session -d -s keep 'sleep 120'\nrun-shell -b 'touch {}'\nrun-shell 'touch {}; while [ ! -f {} ]; do sleep .01; done'\nsource-file /dev/null\nset -g @late 1\n",
            background.display(), ready.display(), go.display()
        )).unwrap();
        let mut starter = server.spawn(&[
            "-f",
            config.to_str().unwrap(),
            "start-server",
            ";",
            "display",
            "-p",
            "starter",
        ]);
        wait(|| ready.exists() && background.exists());
        assert!(starter.0.try_wait().unwrap().is_none());
        starter.0.kill().unwrap();
        starter.0.wait().unwrap();
        std::thread::sleep(Duration::from_millis(100));

        let mut lost = server.spawn(&["display", "-p", "lost"]);
        std::thread::sleep(Duration::from_millis(100));
        if lost.0.try_wait().unwrap().is_some() {
            panic!("{program:?} early lost: {:?}", lost.finish());
        }
        lost.0.kill().unwrap();
        lost.0.wait().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        rejected_startup_clients_release_slots(&server.directory.join("socket"));
        let mut waiting = server.spawn(&[
            "new-session",
            "-d",
            "-s",
            "second",
            "sleep 120",
            ";",
            "display",
            "-p",
            "#{@runs} #{@late}",
        ]);
        std::thread::sleep(Duration::from_millis(100));
        if waiting.0.try_wait().unwrap().is_some() {
            panic!(
                "{program:?} waiting client passed the startup barrier: {:?}",
                waiting.finish()
            );
        }
        std::fs::write(go, b"").unwrap();
        let output = waiting.finish();
        assert!(output.status.success(), "{program:?}: {output:?}");
        assert_eq!(output.stdout, b"x 1\n");
        assert!(output.stderr.is_empty());
        let graph = server
            .spawn(&["list-sessions", "-F", "#{session_name}"])
            .finish();
        assert!(graph.status.success(), "{graph:?}");
        assert_eq!(graph.stdout, b"keep\nsecond\n", "{program:?}");
        let again = server
            .spawn(&["display", "-p", "#{@runs} #{@late}"])
            .finish();
        assert!(again.status.success());
        results.push((output.stdout, graph.stdout, again.stdout));
    }
    assert_eq!(results[0], results[1]);
}
