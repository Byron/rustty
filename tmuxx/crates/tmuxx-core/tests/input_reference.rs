//! Read child input bytes directly, without any terminal rendering normalization.
#[cfg(windows)]
use common::remote_reference;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tmuxx_core::{ClientContext, Effect, Engine};
struct Oracle {
    #[cfg(unix)]
    binary: PathBuf,
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
}
impl Oracle {
    fn run(&self, args: &[&str]) -> Vec<u8> {
        #[cfg(windows)]
        let result = self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        );
        #[cfg(unix)]
        let result = Command::new(&self.binary)
            .args(["-S"])
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }
    fn path(&self, leaf: &str) -> String {
        #[cfg(unix)]
        return self
            .socket
            .parent()
            .unwrap()
            .join(leaf)
            .to_str()
            .unwrap()
            .to_owned();
        #[cfg(windows)]
        return String::from_utf8(self.remote.path(leaf.as_bytes())).unwrap();
    }
    fn wait(&self, leaf: &str) -> Vec<u8> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            #[cfg(unix)]
            if let Ok(bytes) = std::fs::read(self.path(leaf)) {
                return bytes;
            }
            #[cfg(windows)]
            if self.remote.exists(leaf.as_bytes()) {
                return self.remote.get(leaf.as_bytes());
            }
            assert!(Instant::now() < end, "missing C file {leaf}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
#[cfg(unix)]
impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}
fn run(engine: &mut Engine, context: &mut ClientContext, args: &[&str]) -> Vec<u8> {
    let commands = tmuxx_command::parse_argv(args.iter().map(|s| s.as_bytes())).unwrap();
    engine
        .execute(&commands[0], context)
        .unwrap()
        .effects
        .into_iter()
        .filter_map(|effect| {
            if let Effect::Write { data, .. } = effect {
                Some(data)
            } else {
                None
            }
        })
        .flatten()
        .collect()
}
#[test]
#[ignore = "requires C tmux and a raw PTY child"]
fn key_bytes_match_c_across_modes_modifiers_and_backspace_options() {
    #[cfg(unix)]
    let directory = std::env::temp_dir().join(format!(
        "tx-keys-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    #[cfg(unix)]
    std::fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    let oracle = Oracle {
        binary: std::env::var_os("TMUXX_C_TMUX")
            .expect("set TMUXX_C_TMUX")
            .into(),
        socket: directory.join("c"),
    };
    #[cfg(windows)]
    let oracle = Oracle {
        remote: remote_reference::RemoteReference::new(),
    };
    oracle.run(&["new", "-d", "-s", "keys", "sleep 120"]);
    let keys = [
        "a", "C-a", "C-A", "M-a", "S-a", "C-S-a", "C-1", "C-2", "C-3", "C-8", "C-9", "C-0", "C-!",
        "C-@", "C-?", "C-Space", "C-/", "C-\\", "C-\\;", "C-=", "M-C-a", "C-é", "M-é", "C-M-é",
        "é", "Enter", "C-Enter", "M-Enter", "Tab", "S-Tab", "BTab", "C-BTab", "M-Tab", "C-Tab",
        "Escape", "C-Escape", "BSpace", "S-BSpace", "C-BSpace", "M-BSpace", "Home", "End",
        "S-Home", "M-End", "Up", "M-Up", "C-Up", "S-Up", "F1", "S-F1", "M-F1", "F12", "F13", "KP0",
        "KPEnter", "KP/", "M-KP0", "C-KP0", "S-KP0", "PPage", "Delete", "None", "Any", "words",
        "^a", "c-a", "m-s-left", "0x1", "0x7f", "0x41", "pageup", "delete", "[SOH]", "[NUL]",
    ];
    for (index, (policy, mode, format, backspace)) in [
        ("off", b"".as_slice(), "xterm", "C-?"),
        ("off", b"\x1b[>4;2m", "csi-u", "C-?"),
        ("on", b"\x1b[>4;1m", "xterm", "C-?"),
        ("on", b"\x1b[>4;2m", "xterm", "C-?"),
        ("on", b"\x1b[>4;2m", "csi-u", "C-?"),
        ("always", b"", "csi-u", "C-?"),
        ("always", b"\x1b[>4;2m\x1b[>4n", "xterm", "C-h"),
        ("on", b"\x1b[?1h\x1b=", "xterm", "C-h"),
        ("off", b"", "xterm", "a"),
        ("off", b"", "xterm", "M-a"),
        ("off", b"", "xterm", "None"),
    ]
    .into_iter()
    .enumerate()
    {
        let output = format!("bytes-{index}");
        let ready = format!("ready-{index}");
        let script = format!(
            "import os,tty,pathlib\ntty.setraw(0)\nos.write(1,bytes.fromhex('{}'))\npathlib.Path({:?}).write_bytes(b'')\ndata=bytearray()\nwhile True:\n b=os.read(0,1)\n if b==b'\\xff': break\n data.extend(b)\np=pathlib.Path({:?})\np.with_suffix('.tmp').write_bytes(data)\np.with_suffix('.tmp').replace(p)\nwhile True: os.read(0,1)\n",
            mode.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            oracle.path(&ready),
            oracle.path(&output)
        );
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        for (name, value) in [
            ("extended-keys", policy),
            ("extended-keys-format", format),
            ("backspace", backspace),
        ] {
            oracle.run(&["set", "-s", name, value]);
            run(&mut engine, &mut context, &["set", "-s", name, value]);
        }
        oracle.run(&["respawnp", "-k", "python3", "-u", "-c", &script]);
        run(&mut engine, &mut context, &["new", "-d"]);
        engine.feed(0, mode);
        oracle.wait(&ready);
        let mode_format = "#{pane_key_mode}:#{keypad_cursor_flag}:#{keypad_flag}";
        let query =
            tmuxx_command::parse_argv(["display", "-p", mode_format].iter().map(|s| s.as_bytes()))
                .unwrap();
        let expected_mode = engine.execute(&query[0], &mut context).unwrap().output;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let actual = oracle.run(&["display", "-p", mode_format]);
            if actual == expected_mode {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "mode setup: {actual:?} != {expected_mode:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut command = vec!["send", "-t", "%0"];
        command.extend(keys);
        oracle.run(&command);
        let mut expected = run(&mut engine, &mut context, &command);
        for prefix in ["0x1", "0x7f", "None", "Any", "C-A", "S-Tab", "BTab"] {
            for (option, command) in [
                ("prefix", vec!["send-prefix"]),
                ("prefix2", vec!["send-prefix", "-2"]),
            ] {
                oracle.run(&["set", "-g", option, prefix]);
                run(&mut engine, &mut context, &["set", "-g", option, prefix]);
                oracle.run(&command);
                expected.extend(run(&mut engine, &mut context, &command));
            }
        }
        use tmuxx_terminal::{Input, Key, KeyEncodeOptions, KeyEvent, Modifiers};
        for (name, key, modifiers) in [
            (
                "C-a",
                Key::Char('a'),
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                },
            ),
            (
                "M-Up",
                Key::Up,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
            ),
            ("BSpace", Key::Backspace, Modifiers::default()),
            (
                "C-BSpace",
                Key::Backspace,
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                },
            ),
            ("Home", Key::Home, Modifiers::default()),
            (
                "BTab",
                Key::Tab,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            ),
        ] {
            oracle.run(&["send", name]);
            let mut event = KeyEvent::new(key);
            event.modifiers = modifiers;
            let outcome = engine
                .native_input(
                    0,
                    &Input::Key {
                        event,
                        options: KeyEncodeOptions {
                            macos_option_as_alt: true,
                        },
                    },
                )
                .unwrap();
            for effect in outcome.effects {
                if let Effect::Write { data, .. } = effect {
                    expected.extend(data);
                }
            }
        }
        // -F expands copy-mode arguments; ordinary pane input remains literal.
        for command in [
            vec!["send", "-Fl", "#{pane_id}"],
            vec!["send", "-F", "#{pane_id}"],
            vec!["send", "-R"],
            vec!["send", "Up", "a"],
        ] {
            oracle.run(&command);
            expected.extend(run(&mut engine, &mut context, &command));
        }
        assert_eq!(
            oracle.run(&["display", "-p", mode_format]),
            engine
                .expand(mode_format.as_bytes(), Some(0), Some(0), Some(0), &context)
                .unwrap()
                .into_iter()
                .chain(*b"\n")
                .collect::<Vec<_>>()
        );
        oracle.run(&["send", "-H", "ff"]);
        let actual = oracle.wait(&output);
        let first = actual.iter().zip(&expected).position(|(a, b)| a != b);
        assert_eq!(
            actual, expected,
            "policy={policy} mode={mode:?} format={format} backspace={backspace} first mismatch={first:?}"
        );
        let restored = engine.panes[&0].terminal.snapshot().restore().unwrap();
        assert_eq!(
            restored.extended_keys_mode(),
            engine.panes[&0].terminal.extended_keys_mode()
        );
    }
    drop(oracle);
    #[cfg(unix)]
    std::fs::remove_dir_all(directory).unwrap();
}

mod common;
#[test]
#[ignore = "requires C tmux and empty-pane input injection"]
fn send_keys_reset_clears_parser_screen_modes_and_palette_but_keeps_title() {
    let mut pair = common::Pair::new();
    pair.run(&["new-window", "-d", "-n", "empty", ""]);
    pair.run(&["select-pane", "-t", "%1", "-T", "retained title"]);
    let format = "#{pane_title}|#{cursor_x},#{cursor_y}|#{pane_fg}|#{pane_bg}|#{cursor_colour}|#{cursor_shape}|#{cursor_blinking}|#{pane_private_modes}|#{pane_tabs}|#{scroll_region_upper},#{scroll_region_lower}|#{alternate_on}";
    for bytes in [
        b"history\r\nvisible\x1b[?1;6;1003;1006;2004h\x1b[3;12r\x1b[3g\x1b]10;#123456\x07\x1b]11;#654321\x07\x1b]12;#abcdef\x07\x1b[4 q\x1b]unfinished".as_slice(),
        b"\x1b[?1049hALTERNATE\x1b[4h\x1b[?7l\x1b[2;4H\x1b[38;2;255", 
    ] {
        pair.feed("%1", 1, bytes, 1);
        pair.run(&["send-keys", "-t", "%1", "-R"]);
        pair.run(&["display-message", "-p", "-t", "%1", format]);
        pair.run(&["capture-pane", "-p", "-t", "%1", "-S", "-"]);
        pair.run(&["capture-pane", "-pP", "-t", "%1"]);
        let mut terminal = tmuxx_terminal::Terminal::new(80, 24, 2000);
        terminal.feed(bytes);
        terminal.reset_from_host();
        let mut restored = terminal.snapshot().restore().unwrap();
        let capture = tmuxx_terminal::CaptureOptions { pending:true, ..Default::default() };
        assert_eq!(restored.capture_with(capture).unwrap(), terminal.capture_with(capture).unwrap());
        restored.feed(b"after reset");
        assert!(restored.capture().starts_with("after reset"));
        pair.feed("%1", 1, b"after reset", 1);
        pair.run(&["capture-pane", "-p", "-t", "%1"]);
    }
}
