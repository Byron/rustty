//! History is observable state: rows, counters and resets follow the C grid.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
use tmuxx_core::{ClientContext, Engine};

#[cfg(unix)]
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Reference {
    #[cfg(unix)]
    binary: PathBuf,
    #[cfg(unix)]
    directory: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
}
impl Reference {
    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
        #[cfg(windows)]
        let out = self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            input.unwrap_or_default(),
        );
        #[cfg(unix)]
        let out = {
            let mut child = Command::new(&self.binary)
                .args(["-S"])
                .arg(self.directory.join("s"))
                .args(["-f"])
                .arg(self.directory.join("conf"))
                .args(args)
                .env_remove("TMUX")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(input) = input {
                child.stdin.take().unwrap().write_all(input).unwrap();
            }
            child.wait_with_output().unwrap()
        };
        assert!(out.status.success(), "{args:?}: {out:?}");
        out.stdout
    }
    fn new(limit: usize) -> Self {
        #[cfg(unix)]
        let this = {
            let this = Self {
                binary: std::env::var_os("TMUXX_C_TMUX")
                    .expect("set TMUXX_C_TMUX")
                    .into(),
                directory: PathBuf::from(format!(
                    "/tmp/txhistory-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
            };
            std::fs::create_dir(&this.directory).unwrap();
            std::fs::write(
                this.directory.join("conf"),
                format!("set -g history-limit {limit}\nset -g status off\n"),
            )
            .unwrap();
            this
        };
        #[cfg(windows)]
        let this = {
            let remote = remote_reference::RemoteReference::new();
            let output = remote
                .source(format!("set -g history-limit {limit}\nset -g status off\n").as_bytes());
            assert!(output.status.success(), "{output:?}");
            Self { remote }
        };
        this.run(
            &["new-session", "-d", "-x", "40", "-y", "5", "sleep 30"],
            None,
        );
        this.run(
            &[
                "new-window",
                r"stty raw -echo; printf '\033]2;READY\007'; exec cat",
            ],
            None,
        );
        this.wait_title("READY");
        this
    }
    fn wait_title(&self, title: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.run(&["display", "-p", "#{pane_title}"], None) != format!("{title}\n").as_bytes()
        {
            assert!(Instant::now() < deadline, "C pane output timed out");
            std::thread::sleep(Duration::from_millis(3));
        }
    }
    fn feed(&self, bytes: &[u8], index: usize) {
        let title = format!("history-{index}");
        let mut data = bytes.to_vec();
        data.extend_from_slice(format!("\x1b]2;{title}\x07").as_bytes());
        self.run(&["load-buffer", "-b", "input", "-"], Some(&data));
        self.run(&["paste-buffer", "-b", "input", "-drS"], None);
        self.wait_title(&title);
    }
}
#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        self.run(&["kill-server"], None);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn run(engine: &mut Engine, context: &mut ClientContext, args: &[&str]) -> Vec<u8> {
    engine
        .execute(
            &tmuxx_command::Command {
                blocks: Default::default(),
                args: args.iter().map(|v| v.as_bytes().to_vec()).collect(),
            },
            context,
        )
        .unwrap()
        .output
}
#[derive(Debug)]
enum Step {
    Feed(Vec<u8>),
    Limit(usize),
    Resize(u16, u16),
    Scroll(bool),
    Clear(bool),
}
fn check(limit: usize, counters: bool, steps: Vec<Step>) {
    let c = Reference::new(limit);
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    run(
        &mut engine,
        &mut context,
        &["set", "-g", "history-limit", &limit.to_string()],
    );
    run(
        &mut engine,
        &mut context,
        &["new-session", "-d", "-x", "40", "-y", "5", "sleep 30"],
    );
    run(&mut engine, &mut context, &["new-window", ""]);
    let format = if counters {
        "#{history_size}|#{history_limit}|#{history_added}|#{history_collected}|#{history_generation}|#{alternate_on}|#{cursor_x}|#{cursor_y}"
    } else {
        "#{history_size}|#{history_limit}|#{alternate_on}|#{cursor_x}|#{cursor_y}"
    };
    for (index, step) in steps.into_iter().enumerate() {
        match &step {
            Step::Feed(bytes) => {
                c.feed(bytes, index);
                engine.feed(1, bytes);
            }
            Step::Limit(limit) => {
                let args = ["set", "history-limit", &limit.to_string()];
                c.run(&args, None);
                run(&mut engine, &mut context, &args);
            }
            Step::Resize(cols, rows) => {
                let args = [
                    "resize-window",
                    "-x",
                    &cols.to_string(),
                    "-y",
                    &rows.to_string(),
                ];
                c.run(&args, None);
                run(&mut engine, &mut context, &args);
            }
            Step::Scroll(on) => {
                let args = [
                    "set",
                    "-p",
                    "scroll-on-clear",
                    if *on { "on" } else { "off" },
                ];
                c.run(&args, None);
                run(&mut engine, &mut context, &args);
            }
            Step::Clear(hyperlinks) => {
                let args = if *hyperlinks {
                    vec!["clear-history", "-H"]
                } else {
                    vec!["clear-history"]
                };
                c.run(&args, None);
                run(&mut engine, &mut context, &args);
            }
        }
        let actual = run(&mut engine, &mut context, &["display", "-p", format]);
        let expected = c.run(&["display", "-p", format], None);
        assert_eq!(actual, expected, "limit {limit} step {index}: {step:?}");
        let actual = run(
            &mut engine,
            &mut context,
            &["capture-pane", "-p", "-S", "-"],
        );
        let expected = c.run(&["capture-pane", "-p", "-S", "-"], None);
        assert_eq!(
            actual, expected,
            "capture limit {limit} step {index}: {step:?}"
        );
        let terminal = &engine.panes[&1].terminal;
        let restored = terminal.snapshot().restore().unwrap();
        for field in [
            "history_added",
            "history_collected",
            "history_generation",
            "history_limit",
        ] {
            assert_eq!(
                restored.format_value(field),
                terminal.format_value(field),
                "snapshot {field}"
            );
        }
    }
}

#[test]
#[ignore = "requires C tmux and isolated PTYs"]
fn clear_history_clears_primary_history_from_both_screens_like_c() {
    check(
        20,
        true,
        vec![
            Step::Feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n".to_vec()),
            Step::Clear(false),
            Step::Clear(false),
            Step::Feed(b"primary\r\n\x1b[?1049halt\r\n".to_vec()),
            Step::Clear(true),
            Step::Feed(b"\x1b[?1049l".to_vec()),
            Step::Clear(true),
        ],
    );
}
fn steps() -> Vec<Step> {
    let feed = |bytes: &[u8]| Step::Feed(bytes.to_vec());
    let mut steps = vec![feed(b"")];
    for line in 0..30 {
        steps.push(feed(format!("line{line:02}\r\n").as_bytes()));
    }
    steps.extend([
        Step::Resize(40, 3),
        Step::Resize(40, 7),
        Step::Resize(40, 5),
        feed(b"\x1b[2;4r\x1b[4;1Hregion\r\nsecond\r\n\x1b[r"),
        feed(b"\x1b]2;title\x07\x1b[5 q\x1b]9;4;2;88\x07\x1bc"),
        feed(b"primary\r\n\x1b[?1049halt\r\n"),
        feed(b"\x1bc"),
        feed(b"\x1b[3J"),
        feed(b"\x1b[?1049l"),
        feed(b"\x1b[3J"),
        feed(b"\x1b[3J"),
        feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n"),
        Step::Limit(3),
        Step::Limit(3),
        Step::Limit(0),
        feed(b"zero\r\nagain\r\n"),
        Step::Limit(20),
        Step::Scroll(false),
        feed(b"keep\x1bc"),
        Step::Scroll(true),
        feed(b"clear\x1bc"),
        Step::Resize(30, 5),
        Step::Resize(30, 7),
        Step::Resize(40, 7),
    ]);
    steps
}
#[test]
#[ignore = "requires isolated C tmux and TMUXX_C_TMUX"]
fn history_limits_resets_and_regions_match_supported_c() {
    for limit in [0, 1, 3, 20] {
        check(limit, false, steps());
    }
}
#[test]
#[ignore = "requires HEAD C history counter formats and TMUXX_C_TMUX"]
fn head_history_counters_match_c_and_survive_snapshots() {
    for limit in [0, 1, 3, 20] {
        check(limit, true, steps());
    }
}

#[test]
fn native_history_allocation_metrics_account_for_page_capacity() {
    let mut terminal = tmuxx_terminal::Terminal::new(40, 5, 20);
    for bytes in [
        b"".as_slice(),
        "\x1b[31m\x1b]8;;https://example.com\x07e\u{301}\r\n".as_bytes(),
        &b"line\r\n".repeat(200),
    ] {
        terminal.feed(bytes);
        for terminal in [&terminal, &terminal.snapshot().restore().unwrap()] {
            let snapshot = terminal.snapshot();
            let screen = &snapshot.primary;
            let parts = terminal
                .format_value("history_all_bytes")
                .unwrap()
                .split(',')
                .map(|value| value.parse::<usize>().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(parts.len(), 6);
            assert_eq!(parts[1] + parts[3] + parts[5], screen.owned_bytes());
            assert_eq!(
                terminal.format_value("history_bytes").unwrap(),
                screen.owned_bytes().to_string()
            );
            assert!(
                parts[0] >= screen.all_rows().len()
                    && parts[2]
                        >= screen
                            .all_rows()
                            .map(|row| row.cells().len())
                            .sum::<usize>()
            );
            assert_eq!(
                parts[3],
                parts[2] * std::mem::size_of_val(&screen.row(0).cells()[0])
            );
            assert_eq!(parts[4], screen.page_allocations().count());
            assert!(parts[5] > 0);
        }
    }
    let mut snapshot = terminal.snapshot();
    snapshot.primary.tmux_history.as_mut().unwrap().scrolled = usize::MAX;
    assert!(snapshot.restore().is_err());
}

#[test]
#[ignore = "requires an explicit C allocator reference and isolated tmux server"]
fn native_history_allocation_metrics_are_explicit_c_difference() {
    let c = Reference::new(20);
    let mut terminal = tmuxx_terminal::Terminal::new(40, 5, 20);
    for (index, bytes) in [
        b"".as_slice(),
        b"line\r\nline\r\n",
        "\x1b[31me\u{301}".as_bytes(),
    ]
    .into_iter()
    .enumerate()
    {
        c.feed(bytes, index);
        terminal.feed(bytes);
        let expected = c.run(
            &["display", "-p", "#{history_bytes}|#{history_all_bytes}"],
            None,
        );
        let actual = format!(
            "{}|{}\n",
            terminal.format_value("history_bytes").unwrap(),
            terminal.format_value("history_all_bytes").unwrap()
        );
        assert_ne!(
            actual.as_bytes(),
            expected,
            "Native page allocation intentionally differs from C's per-line allocation"
        );
        let expected = String::from_utf8(expected).unwrap();
        let (total, parts) = expected.trim().split_once('|').unwrap();
        let parts = parts
            .split(',')
            .map(|part| part.parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(parts.len(), 6);
        assert_eq!(
            total.parse::<usize>().unwrap(),
            parts[1] + parts[3] + parts[5]
        );
        eprintln!("case{index} C={} Rust={}", expected.trim(), actual.trim());
    }
}
