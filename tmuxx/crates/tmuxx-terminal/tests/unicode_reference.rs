#![cfg(any(unix, windows))]
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
use tmuxx_terminal::{CaptureOptions, Terminal};

struct Reference {
    #[cfg(unix)]
    binary: PathBuf,
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
}
impl Reference {
    fn new(columns: u16) -> Self {
        #[cfg(unix)]
        static NEXT: AtomicU64 = AtomicU64::new(0);
        #[cfg(unix)]
        let reference = Self {
            binary: std::env::var_os("TMUXX_C_TMUX")
                .expect("set TMUXX_C_TMUX")
                .into(),
            socket: std::env::temp_dir().join(format!(
                "tx-unicode-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
        };
        #[cfg(windows)]
        let reference = Self {
            remote: remote_reference::RemoteReference::new(),
        };
        reference.run(&[
            "new-session",
            "-d",
            "-x",
            &columns.to_string(),
            "-y",
            "4",
            "sleep 120",
        ]);
        reference.run(&["new-window", "-E"]);
        reference.run(&["resize-window", "-x", &columns.to_string(), "-y", "4"]);
        reference
    }
    #[cfg(unix)]
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(["-f", "/dev/null", "-S"])
            .arg(&self.socket)
            .env_remove("TMUX")
            .env("LC_ALL", "C.UTF-8");
        command
    }
    fn run(&self, args: &[&str]) -> Vec<u8> {
        #[cfg(unix)]
        let output = self.command().args(args).output().unwrap();
        #[cfg(windows)]
        let output = self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        );
        assert!(output.status.success(), "{args:?}: {output:?}");
        output.stdout
    }
    fn feed(&self, bytes: &[u8]) {
        #[cfg(unix)]
        {
            let mut child = self
                .command()
                .args(["display-message", "-I"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(bytes).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        #[cfg(windows)]
        {
            let output = self.remote.output(&[b"display-message", b"-I"], bytes);
            assert!(output.status.success(), "{output:?}");
        }
    }
}
#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
    }
}

fn content_cells(raw: &str) -> BTreeMap<&str, &str> {
    raw.lines()
        .filter(|line| !line.contains("flags=PADDING") && !line.contains("data=(1,1, )"))
        .filter_map(|line| {
            line.strip_prefix("\t\tC ")
                .and_then(|line| line.split_once(' '))
        })
        .collect()
}

#[test]
#[ignore = "requires current C tmux Unicode input; Windows needs TMUXX_REMOTE_C_CONFIG"]
fn unicode_cells_cursor_and_snapshot_match_c() {
    let overflow = format!("u{}X", "\u{325}".repeat(16));
    for (columns, text) in [
        (10, "🇬🇧🇸🇪🇦X"),
        (10, "🏽👍"),
        (10, "👍🏽"),
        (20, "🏽👍👍👍👍👍👍X"),
        (20, "abc👩🏻‍🚀123"),
        (10, overflow.as_str()),
        (5, "abcd🇬🇧X"),
        (5, "abcd✔️X"),
        (10, "A\u{3164}B"),
        (10, "\u{301}A"),
        (10, "あ\u{301}X"),
        (10, "a\u{200d}あX"),
    ] {
        let reference = Reference::new(columns);
        reference.feed(text.as_bytes());
        let expected = reference.run(&["capture-pane", "-p"]);
        let expected_cursor = reference.run(&["display-message", "-p", "#{cursor_x},#{cursor_y}"]);
        let expected_raw = String::from_utf8(reference.run(&["capture-pane", "-pR"])).unwrap();
        for chunk in [1, 3, text.len()] {
            let mut terminal = Terminal::new(columns, 4, 2000);
            for bytes in text.as_bytes().chunks(chunk) {
                terminal.feed(bytes);
            }
            for terminal in [&terminal, &terminal.snapshot().restore().unwrap()] {
                assert_eq!(
                    terminal.capture_with(CaptureOptions::default()).unwrap(),
                    expected,
                    "capture {text:?}, chunk {chunk}"
                );
                let snapshot = terminal.snapshot();
                let cursor = &snapshot.primary.cursor;
                let cursor = format!(
                    "{},{}\n",
                    cursor.col + usize::from(cursor.pending_wrap),
                    cursor.row
                );
                assert_eq!(
                    cursor.as_bytes(),
                    expected_cursor,
                    "cursor {text:?}, chunk {chunk}"
                );
                let raw = String::from_utf8(
                    terminal
                        .capture_with(CaptureOptions {
                            raw: true,
                            ..Default::default()
                        })
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    content_cells(&raw),
                    content_cells(&expected_raw),
                    "cells {text:?}, chunk {chunk}"
                );
            }
        }
    }
}
