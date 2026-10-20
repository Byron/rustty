//! Effective terminal behavior and replies are observed from a live C pane.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
#[cfg(unix)]
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use std::{
    process::Output,
    time::{Duration, Instant},
};
use tmuxx_core::{ClientContext, Effect, Engine};
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
    fn run(&self, args: &[&str]) -> Vec<u8> {
        let out = self.command(args);
        assert!(out.status.success(), "{args:?}: {out:?}");
        out.stdout
    }
    fn command(&self, args: &[&str]) -> Output {
        #[cfg(windows)]
        return self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        );
        #[cfg(unix)]
        Command::new(&self.binary)
            .args(["-f", "/dev/null", "-S"])
            .arg(self.directory.join("s"))
            .args(args)
            .env_remove("TMUX")
            .env("TERM", "screen")
            .output()
            .unwrap()
    }
    fn new(options: &[(&str, &str)], bytes: &[u8]) -> Self {
        Self::new_pane(options, bytes, false)
    }
    fn new_pane(options: &[(&str, &str)], bytes: &[u8], inactive: bool) -> Self {
        #[cfg(unix)]
        let c = {
            let c = Self {
                binary: std::env::var_os("TMUXX_C_TMUX")
                    .expect("set TMUXX_C_TMUX")
                    .into(),
                directory: PathBuf::from(format!(
                    "/tmp/txtopt-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
            };
            std::fs::create_dir(&c.directory).unwrap();
            c
        };
        #[cfg(windows)]
        let c = Self {
            remote: remote_reference::RemoteReference::new(),
        };
        c.write("input", bytes);
        c.write("child.py", b"import os,sys,tty,time,re\nfrom pathlib import Path\ntty.setraw(0)\np=Path(sys.argv[1]).parent;(p/'ready').touch()\nos.read(0,1)\ndata=(p/'input').read_bytes()+b'\\x1b[c'\nwhile data:data=data[os.write(1,data):]\nreply=b''\nwhile not re.search(rb'\\x1b\\[\\?[0-9;]+c$',reply):reply+=os.read(0,1)\n(p/'reply').write_bytes(re.sub(rb'\\x1b\\[\\?[0-9;]+c$',b'',reply))\ntime.sleep(30)\n");
        c.run(&[
            "new-session",
            "-d",
            "-x",
            "80",
            "-y",
            "24",
            "python3",
            "-B",
            &c.path("child.py"),
            &c.path("input"),
        ]);
        for (name, value) in options {
            c.run(&[
                "set-option",
                if matches!(*name, "codepoint-widths" | "variation-selector-always-wide") {
                    "-s"
                } else {
                    "-p"
                },
                name,
                value,
            ]);
        }
        if inactive {
            c.run(&["split-window", "sleep 30"]);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        for name in ["ready", "reply"] {
            while !c.exists(name) {
                assert!(Instant::now() < deadline, "missing C {name}");
                std::thread::sleep(Duration::from_millis(3));
            }
            if name == "ready" {
                c.run(&["send-keys", "-t", "%0", "-l", "q"]);
            }
        }
        c
    }
    fn path(&self, leaf: &str) -> String {
        #[cfg(unix)]
        return self.directory.join(leaf).to_str().unwrap().to_owned();
        #[cfg(windows)]
        return String::from_utf8(self.remote.path(leaf.as_bytes())).unwrap();
    }
    fn write(&self, leaf: &str, bytes: &[u8]) {
        #[cfg(unix)]
        std::fs::write(self.directory.join(leaf), bytes).unwrap();
        #[cfg(windows)]
        self.remote.put(leaf.as_bytes(), bytes);
    }
    fn exists(&self, leaf: &str) -> bool {
        #[cfg(unix)]
        return self.directory.join(leaf).exists();
        #[cfg(windows)]
        return self.remote.exists(leaf.as_bytes());
    }
    fn read(&self, leaf: &str) -> Vec<u8> {
        #[cfg(unix)]
        return std::fs::read(self.directory.join(leaf)).unwrap();
        #[cfg(windows)]
        return self.remote.get(leaf.as_bytes());
    }
}
#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        self.run(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn compare(options: &[(&str, &str)], bytes: &[u8]) {
    compare_pane(options, bytes, false)
}
fn compare_pane(options: &[(&str, &str)], bytes: &[u8], inactive: bool) {
    let c = if inactive {
        Reference::new_pane(options, bytes, true)
    } else {
        Reference::new(options, bytes)
    };
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    let mut run = |args: &[&str]| {
        engine
            .execute(
                &tmuxx_command::Command {
                    blocks: Default::default(),
                    args: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
                },
                &mut context,
            )
            .unwrap();
    };
    run(&["new-session", "-d", "-x", "80", "-y", "24", ""]);
    for (name, value) in options {
        run(&[
            "set-option",
            if matches!(*name, "codepoint-widths" | "variation-selector-always-wide") {
                "-s"
            } else {
                "-p"
            },
            name,
            value,
        ]);
    }
    if inactive {
        run(&["split-window", ""]);
    }
    let reply = engine
        .feed(0, bytes)
        .into_iter()
        .filter_map(|effect| {
            if let Effect::Write { data, .. } = effect {
                Some(data)
            } else {
                None
            }
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(reply, c.read("reply"), "replies {options:?} input{bytes:?}");
    let state = engine.panes[&0].terminal.snapshot();
    let (screen, alternate) = if state.alternate_active {
        (state.alternate.as_ref().unwrap(), 1)
    } else {
        (&state.primary, 0)
    };
    let cstate = c.run(&[
        "display-message",
        "-p",
        "-t",
        "%0",
        "#{cursor_x},#{cursor_y},#{history_size},#{alternate_on}",
    ]);
    assert_eq!(
        format!(
            "{},{},{},{}\n",
            screen.cursor.col,
            screen.cursor.row,
            screen.history_len(),
            alternate
        )
        .as_bytes(),
        cstate,
        "state {options:?} input{bytes:?}"
    );
    let actual = engine
        .execute(
            &tmuxx_command::Command {
                blocks: Default::default(),
                args: vec![
                    b"capture-pane".to_vec(),
                    b"-p".to_vec(),
                    b"-t".to_vec(),
                    b"%0".to_vec(),
                    b"-S".to_vec(),
                    b"-".to_vec(),
                ],
            },
            &mut context,
        )
        .unwrap()
        .output;
    assert_eq!(
        actual,
        c.run(&["capture-pane", "-p", "-t", "%0", "-S", "-"]),
        "capture {options:?} input{bytes:?}"
    );
}
#[test]
#[ignore = "requires isolated C pane PTYs and TMUXX_C_TMUX"]
fn pane_styles_and_theme_reports_match_c() {
    for inactive in [false, true] {
        for options in [
            vec![("window-style", "fg=#123456,bg=#abcdef")],
            vec![
                ("window-style", "fg=#123456,bg=#abcdef"),
                ("window-active-style", "fg=#fedcba,bg=#010203"),
            ],
        ] {
            compare_pane(&options,b"\x1b]10;?\x07\x1b]11;?\x07\x1b[?996n\x1b]10;#111111\x07\x1b]11;#222222\x07\x1b]10;?\x07\x1b]11;?\x07\x1b[?996n\x1b]110\x07\x1b]111\x07\x1b]10;?\x07\x1b]11;?\x07",inactive);
        }
    }
}
#[test]
#[ignore = "requires isolated C pane PTYs and TMUXX_C_TMUX"]
fn head_unsupported_styles_preserve_inheritance_and_invalid_styles_match_c() {
    let c = Reference::new(&[], b"");
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    engine
        .execute(
            &tmuxx_command::parse(b"new -d ''").unwrap()[0],
            &mut context,
        )
        .unwrap();
    for name in ["window-style", "window-active-style"] {
        for (index, value) in [
            "dim=50%",
            "bold",
            "bg=terminal",
            "#{?pane_active,bg=red,bg=blue}",
        ]
        .into_iter()
        .enumerate()
        {
            c.run(&["set", "-p", name, value]);
            assert_eq!(
                c.run(&["show", "-pv", name]),
                format!("{value}\n").as_bytes()
            );
            let before = engine.snapshot();
            let before_options = engine.panes[&0].options.clone();
            let command =
                tmuxx_command::parse_argv(["set", "-p", name, value].iter().map(|s| s.as_bytes()))
                    .unwrap();
            assert!(
                engine
                    .execute(&command[0], &mut context)
                    .unwrap_err()
                    .contains("unsupported")
            );
            assert_eq!(engine.snapshot(), before);
            assert_eq!(engine.panes[&0].options, before_options);

            // C accepted the local style; remove it before comparing inheritance.
            c.run(&["set", "-pu", name]);
            let inherited = if index % 2 == 0 { "fg=red" } else { "fg=blue" };
            let args = ["set", "-g", name, inherited];
            c.run(&args);
            let command = tmuxx_command::parse_argv(args.iter().map(|s| s.as_bytes())).unwrap();
            engine.execute(&command[0], &mut context).unwrap();
            let args = ["show", "-pA", name];
            let command = tmuxx_command::parse_argv(args.iter().map(|s| s.as_bytes())).unwrap();
            let actual = engine.execute(&command[0], &mut context).unwrap().output;
            assert_eq!(actual, format!("{name}* {inherited}\n").as_bytes());
            assert_eq!(actual, c.run(&args));
        }
        let reset = ["set", "-gu", name];
        c.run(&reset);
        let command = tmuxx_command::parse_argv(reset.iter().map(|s| s.as_bytes())).unwrap();
        engine.execute(&command[0], &mut context).unwrap();
        assert!(!engine.panes[&0].options.contains_key(name));

        let bad = ["set", "-p", name, "bg=nonesuch"];
        let error = c.command(&bad);
        assert!(!error.status.success());
        assert_eq!(error.stderr, b"invalid style: bg=nonesuch\n");
        let command = tmuxx_command::parse_argv(bad.iter().map(|s| s.as_bytes())).unwrap();
        assert_eq!(
            engine.execute(&command[0], &mut context).unwrap_err(),
            "invalid style: bg=nonesuch"
        );
        // Ordinary invalid string writes restore a local inherited value in C.
        assert_eq!(engine.panes[&0].options[name], "default");
        let args = ["show", "-pA", name];
        let command = tmuxx_command::parse_argv(args.iter().map(|s| s.as_bytes())).unwrap();
        let actual = engine.execute(&command[0], &mut context).unwrap().output;
        assert_eq!(actual, format!("{name} default\n").as_bytes());
        assert_eq!(actual, c.run(&args));
    }
}
#[test]
#[ignore = "requires isolated C pane PTYs and TMUXX_C_TMUX"]
fn alternate_scroll_cursor_and_palette_effects_match_c() {
    for value in ["off", "on"] {
        compare(&[("scroll-on-clear", value)], b"one\r\ntwo\x1b[2J");
        compare(&[("scroll-on-clear", value)], b"one\r\ntwo\x1b[H\x1b[J");
        compare(
            &[("alternate-screen", value)],
            b"one\x1b[?1049hTWO\x1b[?1049lEND",
        );
    }
    for value in [
        "default",
        "blinking-block",
        "block",
        "blinking-underline",
        "underline",
        "blinking-bar",
        "bar",
    ] {
        compare(
            &[("cursor-style", value)],
            b"\x1bP$q q\x1b\\\x1b[3 q\x1bP$q q\x1b\\\x1b[0 q\x1bP$q q\x1b\\",
        );
    }
    compare(
        &[("cursor-colour", "#123456")],
        b"\x1b]12;?\x07\x1b]12;#abcdef\x07\x1b]12;?\x07\x1b]112\x07\x1b]12;?\x07",
    );
    for color in ["", "default", "terminal", "red", "#{?pane_active,red,blue}"] {
        compare(
            &[("cursor-colour", color)],
            b"\x1b]12;?\x07\x1b]12;#abcdef\x07\x1b]12;?\x07\x1b]112\x07\x1b]12;?\x07",
        );
    }
    compare(
        &[("pane-colours[5]", "#123456")],
        b"\x1b]4;5;?\x07\x1b]4;5;#abcdef\x07\x1b]4;5;?\x07\x1b]104;5\x07\x1b]4;5;?\x07",
    );
}
#[test]
#[ignore = "requires isolated C pane PTYs and TMUXX_C_TMUX"]
fn codepoint_width_and_variation_selector_options_match_c() {
    for value in [
        "U+00e9=2",
        "é=0",
        "U+754c=1",
        "U+754c-U+754f=1,U+754d=2",
        "invalid=2,U+0000=1,U+754c=9",
    ] {
        compare(&[("codepoint-widths", value)], "aé界界b".as_bytes());
    }
    for value in ["off", "on"] {
        compare(
            &[("variation-selector-always-wide", value)],
            "x\u{fe0f} ♥\u{fe0f} ☺\u{fe0f}".as_bytes(),
        );
    }
}

#[test]
#[ignore = "requires isolated C pane PTYs and TMUXX_C_TMUX"]
fn head_terminal_observable_fields_match_c_and_survive_snapshot() {
    let fields = [
        "insert_flag",
        "wrap_flag",
        "alternate_saved_x",
        "alternate_saved_y",
        "bracket_paste_flag",
        "cursor_blinking",
        "cursor_character",
        "cursor_colour",
        "cursor_shape",
        "cursor_very_visible",
        "image_support",
        "sixel_support",
        "mouse_all_flag",
        "mouse_any_flag",
        "mouse_button_flag",
        "mouse_sgr_flag",
        "mouse_standard_flag",
        "mouse_utf8_flag",
        "origin_flag",
        "pane_bg",
        "pane_fg",
        "pane_path",
        "pane_pb_progress",
        "pane_pb_state",
        "pane_private_modes",
        "pane_synchronized",
        "pane_tabs",
        "pane_unseen_changes",
        "pane_unzoomed_height",
        "pane_unzoomed_width",
        "pane_x",
        "pane_y",
        "scroll_region_lower",
        "scroll_region_upper",
        "synchronized_output_flag",
    ];
    let format = fields
        .iter()
        .map(|field| format!("{field}=#{{{field}}}"))
        .collect::<Vec<_>>()
        .join("|");
    let mut mismatches = Vec::new();
    for (options, bytes) in [
        (vec![], b"".as_slice()),
        (vec![], b"hello\r\x1b[2C".as_slice()),
        (vec![], b"\x1b[4h\x1b[?7l".as_slice()),
        (vec![], b"\x1b[4h\x1b[?7l\x1b[4l\x1b[?7h".as_slice()),
        (vec![], "\u{1b}[2;4H界\u{1b}[4G".as_bytes()),
        (vec![], "\u{1b}[2;4H界\u{1b}[5G".as_bytes()),
        (
            vec![],
            b"\x1b[?1;6;7;12;25;1000;1004;1005;1006;2004;2031h\x1b[34l".as_slice(),
        ),
        (vec![], b"\x1b[?1000h\x1b[?1002h\x1b[?1003h".as_slice()),
        (vec![], b"\x1b[?1002h\x1b[?1000l".as_slice()),
        (vec![], b"\x1b[?12h\x1b[0 q".as_slice()),
        (vec![], b"\x1b[5 q".as_slice()),
        (
            vec![("cursor-style", "blinking-bar"), ("cursor-colour", "red")],
            b"".as_slice(),
        ),
        (
            vec![("cursor-colour", "red")],
            b"\x1b]12;#123456\x07\x1b[4 q".as_slice(),
        ),
        (
            vec![("cursor-colour", "#{?pane_active,red,blue}")],
            b"".as_slice(),
        ),
        (vec![("cursor-colour", "default")], b"".as_slice()),
        (vec![("cursor-colour", "themeblack")], b"".as_slice()),
        (vec![], b"\x1b[4;9H\x1b[?1049h\x1b[2;3H".as_slice()),
        (
            vec![],
            b"\x1b[4;9H\x1b[?1049h\x1b[2;3H\x1b[?1049l".as_slice(),
        ),
        (
            vec![],
            b"\x1b[3g\x1b[7G\x1bH\x1b[20G\x1bH\x1b[3;10r".as_slice(),
        ),
        (
            vec![],
            b"\x1b]7;file://localhost/tmp/example\x07".as_slice(),
        ),
        (vec![], b"\x1b]9;4;1;42\x07".as_slice()),
        (vec![], b"\x1b]9;4;1;42\x07\x1b]9;4;3;77\x07".as_slice()),
        (
            vec![],
            b"\x1b]9;4;2;63\x07\x1b]9;4;4;\x07\x1b]9;4;0\x07".as_slice(),
        ),
        (
            vec![],
            b"\x1b]9;4;1;42\x07\x1b]9;4;4;101\x07\x1b]9;4;2x\x07".as_slice(),
        ),
        (
            vec![],
            b"\x1b]9;4;2;88\x07\x1b[?12h\x1b[34l\x1bc".as_slice(),
        ),
        (
            vec![
                ("synchronize-panes", "on"),
                ("window-style", "fg=colour3,bg=blue"),
            ],
            b"".as_slice(),
        ),
        (
            vec![("window-style", "fg=red,bg=blue")],
            b"\x1b]10;#123456\x07\x1b]11;#abcdef\x07".as_slice(),
        ),
    ] {
        let c = Reference::new(&options, bytes);
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        let mut run = |args: &[&str]| {
            engine
                .execute(
                    &tmuxx_command::Command {
                        blocks: Default::default(),
                        args: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
                    },
                    &mut context,
                )
                .unwrap()
        };
        run(&["new-session", "-d", "-x", "80", "-y", "24", "sleep 30"]);
        for (name, value) in &options {
            run(&["set-option", "-p", name, value]);
        }
        engine.feed(0, bytes);
        let actual = engine
            .execute(
                &tmuxx_command::Command {
                    blocks: Default::default(),
                    args: vec![
                        b"display-message".to_vec(),
                        b"-p".to_vec(),
                        format.as_bytes().to_vec(),
                    ],
                },
                &mut context,
            )
            .unwrap()
            .output;
        let expected = c.run(&["display-message", "-p", "-t", "%0", &format]);
        for (actual, expected) in actual
            .split(|b| *b == b'|')
            .zip(expected.split(|b| *b == b'|'))
        {
            if actual != expected {
                mismatches.push(format!(
                    "options={options:?}, bytes={bytes:?}: {} != {}",
                    String::from_utf8_lossy(actual),
                    String::from_utf8_lossy(expected)
                ));
            }
        }
        let terminal = &engine.panes[&0].terminal;
        let mut restored = terminal.snapshot().restore().unwrap();
        for field in fields {
            assert_eq!(
                restored.format_value(field),
                terminal.format_value(field),
                "restore {field}"
            );
        }
        restored.feed(b"\x1b[?12h\x1b]9;4;2;51\x07");
        assert_eq!(
            restored.format_value("pane_pb_progress").as_deref(),
            Some("51")
        );
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
#[ignore = "requires isolated C pane PTYs and TMUXX_C_TMUX"]
fn head_shell_lifecycle_formats_match_c() {
    let format = "#{pane_command_running}|#{pane_command_status}|#{pane_command_line}|#{?pane_last_prompt_time,yes,no}|#{?pane_command_start_time,yes,no}|#{?pane_command_end_time,yes,no}|#{?pane_command_duration,positive,zero-or-unset}";
    for bytes in [
        b"".as_slice(),
        b"\x1b]133;A\x07",
        b"\x1b]133;N\x07",
        b"\x1b]133;P;k=s\x07",
        b"\x1b]133;B\x07",
        b"\x1b]133;I\x07",
        b"\x1b]133;C;cmdline_url=echo%20hello\x07",
        b"\x1b]133;C;cmdline_url=%23%28touch%20nothing%29%0a%09\x07",
        b"\x1b]133;C;cmdline_url=first\x07\x1b]133;C;cmdline_url=second\x07",
        b"\x1b]133;C;cmdline_url=first\x07\x1b]133;C\x07",
        b"\x1b]133;C;cmdline_url=bad%QQ\x07",
        b"\x1b]133;C;cmdline_url=bad%00tail\x07",
        b"\x1b]133;C;cmdline_url=%ff\x07",
        b"\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;7\x07",
        b"\x1b]133;C\x07\x1b]133;D;0\x07",
        b"\x1b]133;D\x07",
        b"\x1b]133;D;\x07",
        b"\x1b]133;D;key=value\x07",
        b"\x1b]133;D;256\x07",
        b"\x1b]133;D;-1\x07",
        b"\x1b]133;D;abc\x07",
        b"\x1b]133;C;cmdline_url=echo\x07\x1b]133;D;1\x07\x1bc",
    ] {
        let c = Reference::new(&[], bytes);
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        engine
            .execute(
                &tmuxx_command::parse(b"new -d 'sleep 30'").unwrap()[0],
                &mut context,
            )
            .unwrap();
        engine.now = 10_000_000;
        engine.feed(0, bytes);
        let actual = engine
            .execute(
                &tmuxx_command::Command {
                    blocks: Default::default(),
                    args: vec![
                        b"display-message".to_vec(),
                        b"-p".to_vec(),
                        format.as_bytes().to_vec(),
                    ],
                },
                &mut context,
            )
            .unwrap()
            .output;
        assert_eq!(
            actual,
            c.run(&["display-message", "-p", format]),
            "{bytes:?}"
        );
    }
}
