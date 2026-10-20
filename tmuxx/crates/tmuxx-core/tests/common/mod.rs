#[cfg(unix)]
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use tmuxx_core::{ClientContext, Engine};
#[cfg(windows)]
#[path = "../../../../conformance/remote_reference.rs"]
pub(super) mod remote_reference;

pub struct Pair {
    #[cfg(unix)]
    oracle: PathBuf,
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
    engine: Engine,
    context: ClientContext,
}
impl Pair {
    #[allow(dead_code)]
    pub fn feed(&mut self, target: &str, pane: u64, bytes: &[u8], chunk: usize) {
        #[cfg(unix)]
        let output = {
            use std::io::Write;
            use std::process::Stdio;
            let mut child = Command::new(&self.oracle)
                .arg("-S")
                .arg(&self.socket)
                .args(["display-message", "-I", "-t", target])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(bytes).unwrap();
            child.wait_with_output().unwrap()
        };
        #[cfg(windows)]
        let output = self.remote.output(
            &[b"display-message", b"-I", b"-t", target.as_bytes()],
            bytes,
        );
        assert!(
            output.status.success(),
            "feed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        for bytes in bytes.chunks(chunk.max(1)) {
            self.engine.feed(pane, bytes);
        }
    }
    pub fn new() -> Self {
        #[cfg(unix)]
        let oracle = std::env::var_os("TMUXX_C_TMUX")
            .expect("set TMUXX_C_TMUX to the C reference executable")
            .into();
        #[cfg(unix)]
        static NEXT: AtomicU64 = AtomicU64::new(0);
        #[cfg(unix)]
        let socket = PathBuf::from(format!(
            "/tmp/tx-opt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut pair = Self {
            #[cfg(unix)]
            oracle,
            #[cfg(unix)]
            socket,
            #[cfg(windows)]
            remote: remote_reference::RemoteReference::new(),
            engine: Engine::default(),
            context: ClientContext::default(),
        };
        pair.run(&[
            "new-session",
            "-d",
            "-s",
            "main",
            "-n",
            "initial",
            "-x",
            "80",
            "-y",
            "24",
            "sleep 120",
        ]);
        #[cfg(windows)]
        {
            // -D keeps the remote owner alive before its first session; C also
            // changes exit-empty to off. Restore the original default once a
            // live session exists, before querying exact option collections.
            let output = pair
                .remote
                .output(&[b"set-option", b"-g", b"exit-empty", b"on"], &[]);
            assert!(
                output.status.success(),
                "restore C foreground fixture default: {output:?}"
            );
        }
        pair
    }
    pub fn run(&mut self, args: &[&str]) {
        self.compare(args, None);
    }
    #[allow(dead_code)]
    pub fn diagnostic_difference(&mut self, args: &[&str], rust: &str, c: &[&str]) {
        self.compare(args, Some((rust, c)));
    }
    fn compare(&mut self, args: &[&str], diagnostics: Option<(&str, &[&str])>) {
        #[cfg(unix)]
        let expected = Command::new(&self.oracle)
            .env("SHELL", "/bin/sh")
            .env_remove("EDITOR")
            .env_remove("VISUAL")
            .arg("-S")
            .arg(&self.socket)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .output()
            .unwrap();
        #[cfg(windows)]
        let expected = self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        );
        let command = tmuxx_command::parse_argv(args.iter().map(|s| s.as_bytes())).unwrap();
        let actual = self.engine.execute(&command[0], &mut self.context);
        let (success, stdout, stderr) = match actual {
            Ok(outcome) => (true, outcome.output, Vec::new()),
            Err(error) => (false, Vec::new(), format!("{error}\n").into_bytes()),
        };
        assert_eq!(
            success,
            expected.status.success(),
            "status {args:?}: {}",
            String::from_utf8_lossy(&expected.stderr)
        );
        if stdout != expected.stdout {
            let actual = String::from_utf8_lossy(&stdout);
            let wanted = String::from_utf8_lossy(&expected.stdout);
            let mismatch = actual
                .lines()
                .zip(wanted.lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| actual.lines().count().min(wanted.lines().count()));
            panic!(
                "stdout {args:?} at line {mismatch}: actual {:?}, expected {:?}; byte lengths {} and {}",
                actual.lines().nth(mismatch),
                wanted.lines().nth(mismatch),
                stdout.len(),
                expected.stdout.len()
            );
        }
        if let Some((rust, c)) = diagnostics {
            assert!(!success, "expected rejection: {args:?}");
            assert_eq!(stderr, rust.as_bytes());
            assert!(
                c.iter()
                    .any(|diagnostic| diagnostic.as_bytes() == expected.stderr),
                "C diagnostic for {args:?}: {:?}",
                String::from_utf8_lossy(&expected.stderr)
            );
        } else {
            assert_eq!(stderr, expected.stderr, "stderr {args:?}");
        }
    }
}
#[cfg(unix)]
impl Drop for Pair {
    fn drop(&mut self) {
        let _ = Command::new(&self.oracle)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}
