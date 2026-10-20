mod common;
use common::Pair;

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn creation_styles_and_contextless_geometry_flags_match_c() {
    let mut p = Pair::new();
    for args in [
        vec![
            "split-window",
            "-dE",
            "-s",
            "fg=#123456,bg=#abcdef",
            "-t",
            "%0",
        ],
        vec!["show-options", "-p", "-t", "%1", "window-style"],
        vec!["show-options", "-p", "-t", "%1", "window-active-style"],
        vec!["new-pane", "-dE", "-s", "fg=red,bg=blue", "-t", "%0"],
        vec!["show-options", "-p", "-t", "%2", "window-style"],
        vec!["show-options", "-p", "-t", "%2", "window-active-style"],
        vec!["display-message", "-p", "-t", "%2", "#{pane_fg}|#{pane_bg}"],
        vec!["select-pane", "-t", "%2"],
        vec!["display-message", "-p", "-t", "%2", "#{pane_fg}|#{pane_bg}"],
        vec!["resize-pane", "-MZ", "-x", "invalid", "-t", "%0"],
        vec![
            "move-pane",
            "-M",
            "-s",
            "%0",
            "-t",
            "%0",
            "-P",
            "invalid",
            "-X",
            "invalid",
        ],
        vec!["movep", "-M", "-s", "%1", "-t", "%0", "-l", "invalid"],
        vec!["move-pane", "-M", "-s", "%999", "-t", "%0"],
        vec!["move-pane", "-M", "-s", "%0", "-t", "%999"],
        vec!["new-pane", "-M", "-x", "invalid", "-t", "%0"],
        vec!["new-pane", "-LME", "-d", "-t", "%0"],
        vec![
            "break-pane",
            "-d",
            "-s",
            "%1",
            "-t",
            "main:8",
            "-x",
            "invalid",
            "-y",
            "-2",
            "-X",
            "nonsense",
            "-Y",
            "9999",
        ],
    ] {
        p.run(&args);
        p.run(&["list-panes", "-a", "-F", "#{session_name}:#{window_index}:#{pane_id}:#{pane_active}:#{pane_left},#{pane_top},#{pane_width},#{pane_height}"]);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn resize_trim_history_and_flag_precedence_match_c() {
    let mut p = Pair::new();
    p.run(&["new-window", "-dE", "-t", "main:1"]);
    p.run(&["resize-window", "-t", "main:1", "-x", "12", "-y", "4"]);
    for bytes in [
        b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\x1b[2;2H".as_slice(),
        b"\x1b[H\x1b[31mZ".as_slice(),
        b"\x1b[?1049hother\x1b[H".as_slice(),
        b"\x1b[?1049l\x1b[H".as_slice(),
    ] {
        // C's spare allocation exposed by -N depends on input batching.
        p.feed("%1", 1, bytes, bytes.len());
        p.run(&["capture-pane", "-peN", "-t", "%1", "-S", "-", "-E", "-"]);
        for _ in 0..2 {
            p.run(&["resize-pane", "-TMZ", "-x", "invalid", "-t", "%1"]);
            p.run(&["display-message", "-p", "-t", "%1", "#{pane_width}|#{pane_height}|#{cursor_x}|#{cursor_y}|#{history_size}|#{history_added}|#{history_collected}|#{history_generation}|#{alternate_on}|#{window_zoomed_flag}"]);
            p.run(&["capture-pane", "-p", "-t", "%1", "-S", "-", "-E", "-"]);
            p.run(&["capture-pane", "-peN", "-t", "%1", "-S", "-", "-E", "-"]);
        }
    }
}

#[test]
fn native_creation_styles_and_history_trim_survive_snapshots() {
    use tmuxx_core::{ClientContext, Engine};
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    for command in ["new -dE", "split-window -dE -s 'fg=#123456,bg=#abcdef'"] {
        engine
            .execute(
                &tmuxx_command::parse(command.as_bytes()).unwrap()[0],
                &mut context,
            )
            .unwrap();
    }
    let value = &engine.snapshot()["panes"][1]["terminal"];
    let snapshot: tmuxx_terminal::Snapshot = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(snapshot.foreground, [0x12, 0x34, 0x56]);
    assert_eq!(snapshot.background, [0xab, 0xcd, 0xef]);
    let mut terminal = tmuxx_terminal::Terminal::new(12, 4, 100);
    terminal.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\x1b[2;2H");
    terminal.trim_history_below_cursor();
    assert_eq!(terminal.cursor(), (1, 3));
    let mut restored = terminal.snapshot().restore().unwrap();
    for bytes in [b"Z".as_slice(), b"\r\nnext", b"\x1b[?1049hALT\x1b[?1049l"] {
        terminal.feed(bytes);
        restored.feed(bytes);
        assert_eq!(restored.capture(), terminal.capture());
        assert_eq!(restored.cursor(), terminal.cursor());
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn creation_presentation_flags_are_explicit_no_mutation_differences() {
    #[cfg(unix)]
    use std::{path::PathBuf, process::Command};
    use tmuxx_core::{ClientContext, Engine};
    struct Server {
        #[cfg(unix)]
        binary: PathBuf,
        #[cfg(unix)]
        socket: PathBuf,
        #[cfg(windows)]
        remote: common::remote_reference::RemoteReference,
    }
    impl Server {
        fn run(&self, args: &[&str]) -> Vec<u8> {
            #[cfg(unix)]
            let output = Command::new(&self.binary)
                .args(["-f", "/dev/null", "-S"])
                .arg(&self.socket)
                .args(args)
                .output()
                .unwrap();
            #[cfg(windows)]
            let output = self.remote.output(
                &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
                &[],
            );
            assert!(output.status.success(), "{args:?}: {output:?}");
            output.stdout
        }
    }
    #[cfg(unix)]
    impl Drop for Server {
        fn drop(&mut self) {
            self.run(&["kill-server"]);
        }
    }
    #[cfg(unix)]
    let c = Server {
        binary: std::env::var_os("TMUXX_C_TMUX")
            .expect("set TMUXX_C_TMUX")
            .into(),
        socket: PathBuf::from(format!("/tmp/tmuxx-pane-flags-{}", std::process::id())),
    };
    #[cfg(windows)]
    let c = Server {
        remote: common::remote_reference::RemoteReference::new(),
    };
    c.run(&["new-session", "-dE"]);
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    engine
        .execute(
            &tmuxx_command::parse(b"new-session -dE").unwrap()[0],
            &mut context,
        )
        .unwrap();
    for command in ["split-window", "new-pane"] {
        for (flag, value) in [
            ("-m", "process ended"),
            ("-S", "fg=red"),
            ("-R", "fg=blue"),
            ("-s", "bold"),
            ("-s", "bg=terminal"),
            ("-s", "fg=invalid"),
            ("-s", "bg=#zzzzzz"),
        ] {
            let args = [command, "-dEP", "-F", "#{pane_id}", flag, value];
            let before = engine.snapshot();
            let error = engine
                .execute(
                    &tmuxx_command::parse_argv(args.iter().map(|arg| arg.as_bytes())).unwrap()[0],
                    &mut context,
                )
                .unwrap_err();
            assert!(error.contains("unsupported"), "{args:?}: {error}");
            assert_eq!(engine.snapshot(), before, "{args:?} mutated native state");
            let created = c.run(&args);
            let id = std::str::from_utf8(&created).unwrap().trim();
            assert!(id.starts_with('%'));
            c.run(&["kill-pane", "-t", id]);
        }
    }
}
