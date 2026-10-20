//! Exact capture bytes from isolated stock tmux processes, never golden files.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
use std::process::Output;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tmuxx_terminal::{CaptureOptions, Terminal};

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
    fn start(bytes: &[u8], cols: u16, rows: u16) -> Self {
        #[cfg(unix)]
        let reference = {
            let binary = std::env::var_os("TMUXX_C_TMUX")
                .expect("set TMUXX_C_TMUX")
                .into();
            let directory = PathBuf::from(format!(
                "/tmp/txcap-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self { binary, directory }
        };
        #[cfg(windows)]
        let reference = Self {
            remote: remote_reference::RemoteReference::new(),
        };
        let payload = bytes
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect::<String>();
        let child = format!("stty raw -echo; printf '{payload}'; exec sleep 60");
        let output = reference.command(&[
            "new-session",
            "-d",
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
            &child,
        ]);
        assert!(output.status.success(), "{output:?}");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = reference.command(&["capture-pane", "-p", "-S", "-"]);
            if output.stdout.windows(5).any(|word| word == b"READY") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "reference did not produce fixture: {output:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        reference
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
            .env("LC_ALL", "C.UTF-8")
            .output()
            .unwrap()
    }
    fn compare(&self, terminal: &Terminal, flags: &str, options: CaptureOptions) {
        let mut args = vec!["capture-pane", flags];
        let start = options.start.map(|value| value.to_string());
        let end = options.end.map(|value| value.to_string());
        if let Some(value) = start.as_deref() {
            args.extend(["-S", value]);
        }
        if let Some(value) = end.as_deref() {
            args.extend(["-E", value]);
        }
        let reference = self.command(&args);
        let actual = terminal.capture_with(options);
        if !reference.status.success() {
            assert_eq!(
                format!("{}\n", actual.unwrap_err()).as_bytes(),
                reference.stderr,
                "{args:?}"
            );
        } else {
            let mut actual = actual.unwrap();
            if actual.last() != Some(&b'\n') {
                actual.push(b'\n');
            }
            assert_eq!(
                actual,
                reference.stdout,
                "{args:?}; actual={:?}, C={:?}",
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(&reference.stdout)
            );
        }
    }
}
#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        self.command(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn fixtures(bytes: &[u8], cols: u16, rows: u16) -> (Reference, Terminal) {
    let reference = Reference::start(bytes, cols, rows);
    let mut terminal = Terminal::new(cols, rows, 2000);
    terminal.feed(bytes);
    (reference, terminal)
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn retained_tabs_capture_overwrite_erase_shift_and_snapshot_match_c() {
    for payload in [
        "\tX",
        "a\tb",
        "\t\tZ",
        "\x1b[3g\tZ",
        "\tX\rZ",
        "\tX\r\x1b[4GZ",
        "\tX\r\x1b[8GZ",
        "\tX\r\x1b[4G\x1b[K",
        "\tX\r\x1b[4G\x1b[1K",
        "\tX\r\x1b[4G\x1b[2X",
        "\tX\r\x1b[4G\x1b[2P",
        "\tX\r\x1b[4G\x1b[2@",
        "\tX\r\x1b[2@",
        "\x1b[31;44m        \x1b[0m\r\tX",
        "\x1b[31;44m        \x1b[0m\r\tX\r\x1b[4GZ",
        "  X     \r\tY",
        "\x1b[31m  \x1b[0m      \r\tY",
        "\x1b]8;id=tab;https://example.com\x1b\\        \x1b]8;;\x1b\\\r\tX",
        "\x1b[2IY",
        "\tX\r界",
        "\tX\r\x1b[4G界",
    ] {
        let bytes = format!("READY\r\n{payload}").into_bytes();
        let (reference, terminal) = fixtures(&bytes, 20, 4);
        let restored = terminal.snapshot().restore().unwrap();
        for (flags, mut options) in [
            ("-p", CaptureOptions::default()),
            (
                "-pN",
                CaptureOptions {
                    preserve_trailing: true,
                    ..Default::default()
                },
            ),
            (
                "-pe",
                CaptureOptions {
                    escape: true,
                    ..Default::default()
                },
            ),
            (
                "-peNTF",
                CaptureOptions {
                    escape: true,
                    preserve_trailing: true,
                    trim_unused: true,
                    flags: true,
                    ..Default::default()
                },
            ),
        ] {
            options.start = Some(-100);
            reference.compare(&terminal, flags, options);
            assert_eq!(
                restored.capture_with(options),
                terminal.capture_with(options),
                "{payload:?}"
            );
        }
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn retained_tabs_reflow_at_normal_widths_matches_c() {
    for bytes in [
        b"READY\r\n\tX\r\na\tb".as_slice(),
        b"READY\r\n01234567890123456789A\tX\r\n\t\tY".as_slice(),
        b"READY\r\n\x1b[44m        \x1b[0m\r\tX\r\nend".as_slice(),
    ] {
        let (reference, mut terminal) = fixtures(bytes, 20, 4);
        for width in [20, 8, 32, 10, 20] {
            assert!(
                reference
                    .command(&["resize-window", "-x", &width.to_string()])
                    .status
                    .success()
            );
            terminal.resize(width, 4);
            for flags in ["-p", "-pN", "-peNT", "-peNJ", "-pNL"] {
                let options = CaptureOptions {
                    start: Some(i32::MIN),
                    preserve_trailing: flags.contains('N'),
                    escape: flags.contains('e'),
                    trim_unused: flags.contains('T'),
                    join: flags.contains('J'),
                    number_lines: flags.contains('L'),
                    ..Default::default()
                };
                reference.compare(&terminal, flags, options);
                reference.compare(&terminal.snapshot().restore().unwrap(), flags, options);
            }
        }
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn native_reflow_expands_oversized_and_fractured_tabs_without_hidden_cells() {
    for (bytes, widths, narrow_c, narrow_native) in [
        (
            b"READY\r\n\tX\r\n".as_slice(),
            vec![8, 6],
            b"READY\n\t\n\n\n".as_slice(),
            b"READY\n\n  X\n\n".as_slice(),
        ),
        (
            b"READY\r\n\tX\r\x1b[4G\x1b[2X\r\n".as_slice(),
            vec![10],
            b"READY\n\t\nX\n\n".as_slice(),
            b"READY\n        X\n\n".as_slice(),
        ),
    ] {
        let (reference, mut terminal) = fixtures(bytes, 20, 3);
        let options = CaptureOptions {
            start: Some(i32::MIN),
            ..Default::default()
        };
        for width in widths {
            assert!(
                reference
                    .command(&["resize-window", "-x", &width.to_string()])
                    .status
                    .success()
            );
            terminal.resize(width, 3);
            assert!(
                terminal
                    .snapshot()
                    .primary
                    .all_rows()
                    .all(|row| row.cells.len() <= usize::from(width))
            );
        }
        assert_eq!(
            reference.command(&["capture-pane", "-p", "-S", "-"]).stdout,
            narrow_c
        );
        assert_eq!(terminal.capture_with(options).unwrap(), narrow_native);
        assert!(terminal.capture_with(options).unwrap().contains(&b'X'));
        let mut restored = terminal.snapshot().restore().unwrap();
        assert_eq!(
            restored.capture_with(options),
            terminal.capture_with(options)
        );
        assert!(
            reference
                .command(&["resize-window", "-x", "20"])
                .status
                .success()
        );
        terminal.resize(20, 3);
        restored.resize(20, 3);
        assert_eq!(
            terminal.capture_with(options).unwrap(),
            b"READY\n        X\n\n"
        );
        assert_eq!(
            restored.capture_with(options),
            terminal.capture_with(options)
        );
        assert!(
            reference
                .command(&["capture-pane", "-p", "-S", "-"])
                .stdout
                .contains(&b'X')
        );
    }
}

#[test]
fn malformed_utf8_strings_and_tabs_resume_after_every_snapshot_boundary() {
    let bytes = b"\xf0\x80\x80\x80A\xed\xa0\x80B\xe2\xc2\xa3C\xe2D\xc2\xa3\r\n\tX\x1b]8;id=a;https://example.com\x1b\\L\x1b]8;id=b:id=c;https://invalid\x1b\\M\x1b]8;;\x1b\\\x1bP$qBAD\x1b\\";
    let options = CaptureOptions {
        escape: true,
        preserve_trailing: true,
        ..Default::default()
    };
    let mut reference = Terminal::new(40, 4, 20);
    let effects = reference.feed(bytes);
    for split in 0..=bytes.len() {
        let mut terminal = Terminal::new(40, 4, 20);
        let mut actual_effects = terminal.feed(&bytes[..split]);
        let saved = serde_json::to_vec(&terminal.snapshot()).unwrap();
        let saved: tmuxx_terminal::Snapshot = serde_json::from_slice(&saved).unwrap();
        let mut terminal = saved.restore().unwrap();
        actual_effects.extend(terminal.feed(&bytes[split..]));
        assert_eq!(actual_effects, effects, "effects at byte {split}");
        assert_eq!(
            terminal.capture_with(options),
            reference.capture_with(options),
            "capture at byte {split}"
        );
        assert_eq!(
            terminal.cursor(),
            reference.cursor(),
            "cursor at byte {split}"
        );
    }
}

#[test]
fn invalid_snapshot_tab_metadata_is_rejected() {
    let mut terminal = Terminal::new(20, 3, 20);
    terminal.feed(b"\tX");
    for (col, marker) in [(0, 33), (1, -2), (8, 1)] {
        let mut snapshot = terminal.snapshot();
        let mut style = snapshot.primary.row(0).style(col);
        style.tmux_tab = marker;
        snapshot.primary.set_cell_style(0, col, style);
        assert!(
            snapshot.restore().is_err(),
            "accepted tab marker {marker} at {col}"
        );
    }
    let mut snapshot = terminal.snapshot();
    snapshot.primary.cursor.style.tmux_tab = 1;
    assert!(snapshot.restore().is_err());
}

#[test]
fn snapshot_json_retains_row_flags_times_and_rejects_duplicate_ids() {
    let mut terminal = Terminal::new(20, 2, 20);
    terminal.feed_at(b"\tX\r\ny\r\nz", 12345);
    let snapshot = terminal.snapshot();
    let wire = serde_json::to_string(&snapshot).unwrap();
    let encoded = serde_json::to_value(&snapshot).unwrap();
    assert!(
        !encoded["primary"]["tmux_row_times"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let restored: tmuxx_terminal::Snapshot = serde_json::from_str(&wire).unwrap();
    let options = CaptureOptions {
        timestamps: true,
        flags: true,
        start: Some(i32::MIN),
        ..Default::default()
    };
    assert_eq!(
        restored.restore().unwrap().capture_with(options),
        terminal.capture_with(options)
    );
    for replacement in [
        "{\"0\":2,\"0\":2}",
        "{\"00\":2}",
        "{\"18446744073709551616\":2}",
    ] {
        let invalid = wire.replace(
            "\"tmux_row_flags\":{\"0\":2}",
            &format!("\"tmux_row_flags\":{replacement}"),
        );
        assert_ne!(invalid, wire);
        assert!(serde_json::from_str::<tmuxx_terminal::Snapshot>(&invalid).is_err());
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_sgr_color_origins_spaces_escaping_and_ranges_match_c() {
    let bytes = b"READY  \r\n\x1b[31mRED\x1b[0m\r\n\x1b[1;38;5;1mB\x1b[0m!\\\r\n\x1b[38;2;12;34;56;48;5;200mRGB\x1b[0m\r\n\x1b[4:3;58;2;1;2;3mUNDER\x1b[0m\r\nlast";
    let (reference, terminal) = fixtures(bytes, 20, 5);
    for (flags, options) in [
        ("-p", CaptureOptions::default()),
        ("-pM", CaptureOptions::default()),
        (
            "-pL",
            CaptureOptions {
                number_lines: true,
                ..Default::default()
            },
        ),
        (
            "-pN",
            CaptureOptions {
                preserve_trailing: true,
                ..Default::default()
            },
        ),
        (
            "-peN",
            CaptureOptions {
                escape: true,
                preserve_trailing: true,
                ..Default::default()
            },
        ),
        (
            "-pe",
            CaptureOptions {
                escape: true,
                ..Default::default()
            },
        ),
        (
            "-pC",
            CaptureOptions {
                escape_sequences: true,
                ..Default::default()
            },
        ),
        (
            "-peC",
            CaptureOptions {
                escape: true,
                escape_sequences: true,
                ..Default::default()
            },
        ),
        (
            "-peT",
            CaptureOptions {
                escape: true,
                trim_unused: true,
                ..Default::default()
            },
        ),
        (
            "-pNT",
            CaptureOptions {
                preserve_trailing: true,
                trim_unused: true,
                ..Default::default()
            },
        ),
        (
            "-pJ",
            CaptureOptions {
                join: true,
                ..Default::default()
            },
        ),
    ] {
        reference.compare(&terminal, flags, options);
        reference.compare(
            &terminal,
            flags,
            CaptureOptions {
                start: Some(-1000),
                end: Some(1),
                ..options
            },
        );
        reference.compare(
            &terminal,
            flags,
            CaptureOptions {
                start: Some(4),
                end: Some(1),
                ..options
            },
        );
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_hyperlink_lists_numbers_and_flag_precedence_match_c() {
    let bytes = b"READY\r\n\x1b]8;id=a;https://one\x1b\\ab\r\ncd\x1b]8;;\x1b\\\x1b]8;id=b;https://two\x1b\\ef\x1b]8;;\x1b\\\r\n\x1b]8;id=c;https://one\x1b\\gh\x1b]8;;\x1b\\\r\n\x1b]8;;https://implicit\x1b\\123456789012345678901234567890\x1b]8;;\x1b\\\r\nlast";
    let (reference, terminal) = fixtures(bytes, 20, 5);
    for flags in ["-pH", "-pHL", "-pHJ", "-pHeC", "-pHPL", "-pMHL", "-pHLF"] {
        for start in [None, Some(i32::MIN), Some(1)] {
            let options = CaptureOptions {
                hyperlinks: true,
                number_lines: flags.contains('L'),
                flags: flags.contains('F'),
                join: flags.contains('J'),
                pending: flags.contains('P'),
                start,
                ..Default::default()
            };
            reference.compare(&terminal, flags, options);
            reference.compare(&terminal.snapshot().restore().unwrap(), flags, options);
        }
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_line_flags_follow_shell_wrap_hyperlink_and_extended_lifetimes() {
    for bytes in [
        b"READY\r\n\x1b]133;A\x07prompt\x1b]133;B\x07command\r\n\x1b]133;C\x07output\x1b]133;D;3\x07\r\n012345678901234567890wrap".as_slice(),
        "READY\r\n\x1b]8;id=1;https://one\x1b\\link\x1b]8;;\x1b\\\rPLAIN\r\n中\rX\r\n\x1b[38;2;1;2;3mRGB\x1b[0m\rX".as_bytes(),
        "READY\r\n中\x1b[2K\rblank\r\n\x1b[4:2munderline\x1b[0m\rX\r\n\x1b[9;53mstrike\x1b[0m\rX".as_bytes(),
    ] {
        let (reference, mut terminal) = fixtures(bytes, 20, 5);
        for width in [20, 8, 30] {
            assert!(reference.command(&["resize-window", "-x", &width.to_string()]).status.success());
            terminal.resize(width, 5);
            for (flags, join) in [("-pLF",false), ("-pLFJ",true)] {
                let options = CaptureOptions { number_lines:true, flags:true, join, start:Some(i32::MIN), ..Default::default() };
                reference.compare(&terminal,flags,options);
                reference.compare(&terminal.snapshot().restore().unwrap(),flags,options);
            }
        }
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn clear_history_hyperlinks_keep_text_and_remove_saved_screen_links_like_c() {
    for alternate in [false, true] {
        let mut bytes = b"READY\r\n\x1b]8;id=a;https://one\x1b\\linked\r\nmore\r\nlast".to_vec();
        if alternate {
            bytes.extend_from_slice(b"\x1b[?1049hALTREADY\x1b]8;id=b;https://alt\x1b\\alternate");
        }
        let (reference, mut terminal) = fixtures(&bytes, 20, 3);
        for clear_links in [false, true] {
            let args = if clear_links {
                vec!["clear-history", "-H"]
            } else {
                vec!["clear-history"]
            };
            assert!(reference.command(&args).status.success());
            terminal.clear_history(clear_links);
            for (flags, options) in [
                (
                    "-pe",
                    CaptureOptions {
                        escape: true,
                        ..Default::default()
                    },
                ),
                (
                    "-pH",
                    CaptureOptions {
                        hyperlinks: true,
                        ..Default::default()
                    },
                ),
                (
                    "-pae",
                    CaptureOptions {
                        alternate: true,
                        escape: true,
                        ..Default::default()
                    },
                ),
            ] {
                reference.compare(&terminal, flags, options);
                reference.compare(&terminal.snapshot().restore().unwrap(), flags, options);
            }
        }
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_line_timestamps_follow_history_entry_and_reflow() {
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let before = epoch();
    let bytes = b"READY\r\none-two-three-four\r\nline2\r\nline3\r\nline4\r\nline5\r\nlast";
    let reference = Reference::start(bytes, 20, 4);
    let after = epoch();
    let mut terminal = Terminal::new(20, 4, 2000);
    terminal.feed_at(bytes, 123456);
    for cols in [20, 8, 32] {
        assert!(
            reference
                .command(&["resize-window", "-x", &cols.to_string()])
                .status
                .success()
        );
        terminal.resize(cols, 4);
        let c = reference.command(&["capture-pane", "-pLI", "-S", "-"]);
        assert!(c.status.success(), "{c:?}");
        let options = CaptureOptions {
            number_lines: true,
            timestamps: true,
            start: Some(i32::MIN),
            ..Default::default()
        };
        let actual = terminal.capture_with(options).unwrap();
        assert_eq!(
            actual,
            terminal
                .snapshot()
                .restore()
                .unwrap()
                .capture_with(options)
                .unwrap()
        );
        let parse = |data: &[u8], native: bool| {
            String::from_utf8(data.to_vec())
                .unwrap()
                .lines()
                .map(|line| {
                    let mut parts = line.splitn(3, ' ');
                    let index = parts.next().unwrap().parse::<i32>().unwrap();
                    let time = parts.next().unwrap().parse::<u64>().unwrap();
                    if time != 0 {
                        if native {
                            assert_eq!(time, 123456);
                        } else {
                            assert!(
                                (before..=after).contains(&time),
                                "{time} outside {before}..={after}"
                            );
                        }
                    }
                    (index, time != 0, parts.next().unwrap().to_owned())
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parse(&actual, true),
            parse(&c.stdout, false),
            "width {cols}"
        );
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_alternate_history_and_missing_screen_match_c() {
    let (reference, terminal) = fixtures(
        b"READY\r\none\r\ntwo\r\nthree\x1b[?1049h\x1b[HALTREADY",
        20,
        3,
    );
    for (flags, alternate) in [("-p", false), ("-pa", true)] {
        for start in [None, Some(-1000), Some(1)] {
            reference.compare(
                &terminal,
                flags,
                CaptureOptions {
                    alternate,
                    start,
                    ..Default::default()
                },
            );
        }
    }
    drop(reference);
    let (reference, terminal) = fixtures(b"READY\x1b[?1049hALT\x1b[?1049l", 20, 3);
    reference.compare(
        &terminal,
        "-pa",
        CaptureOptions {
            alternate: true,
            ..Default::default()
        },
    );
    reference.compare(
        &terminal,
        "-paq",
        CaptureOptions {
            alternate: true,
            quiet: true,
            ..Default::default()
        },
    );
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_pending_input_raw_and_octal_match_c() {
    for bytes in [
        b"READY\x1b[38;5;".as_slice(),
        b"READY\x1b]2;unfinished\\\x01\xff",
        b"READY\x1b[1\x07;",
        b"READY\x1b[1\x1b[2;",
        b"READY\xc3",
    ] {
        let (reference, terminal) = fixtures(bytes, 20, 3);
        reference.compare(
            &terminal,
            "-pP",
            CaptureOptions {
                pending: true,
                ..Default::default()
            },
        );
        reference.compare(
            &terminal,
            "-pPC",
            CaptureOptions {
                pending: true,
                escape_sequences: true,
                ..Default::default()
            },
        );
        let restored = terminal.snapshot().restore().unwrap();
        reference.compare(
            &restored,
            "-pPC",
            CaptureOptions {
                pending: true,
                escape_sequences: true,
                ..Default::default()
            },
        );
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_allocations_after_erase_insert_delete_and_scroll_match_c() {
    let (reference, terminal) = fixtures(b"READY\r\nabcdefghijklmnop\r\x1b[5G\x1b[KX\r\nabcdefghijklmnop\r\x1b[2K\x1b[31mNEW\x1b[0m\r\nabcdef\r\x1b[3G\x1b[2@I\r\nabcdef\r\x1b[3G\x1b[2PD\r\nlast", 20, 5);
    for (flags, options) in [
        (
            "-pN",
            CaptureOptions {
                preserve_trailing: true,
                ..Default::default()
            },
        ),
        (
            "-peN",
            CaptureOptions {
                escape: true,
                preserve_trailing: true,
                ..Default::default()
            },
        ),
        (
            "-pNT",
            CaptureOptions {
                preserve_trailing: true,
                trim_unused: true,
                ..Default::default()
            },
        ),
        (
            "-pe",
            CaptureOptions {
                escape: true,
                ..Default::default()
            },
        ),
    ] {
        let options = CaptureOptions {
            start: Some(i32::MIN),
            ..options
        };
        reference.compare(&terminal, flags, options);
        let restored = terminal.snapshot().restore().unwrap();
        reference.compare(&restored, flags, options);
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_background_erase_and_scroll_colors_match_c() {
    let (reference, terminal) = fixtures(b"READY\r\n\x1b[41m\x1b[2K\r\n\x1b[48;5;1m\x1b[2K\r\n\x1b[42mcolored\r\nscroll\r\n\x1b[0mLAST", 20, 4);
    for (flags, options) in [
        (
            "-pe",
            CaptureOptions {
                escape: true,
                ..Default::default()
            },
        ),
        (
            "-peN",
            CaptureOptions {
                escape: true,
                preserve_trailing: true,
                ..Default::default()
            },
        ),
        (
            "-peNT",
            CaptureOptions {
                escape: true,
                preserve_trailing: true,
                trim_unused: true,
                ..Default::default()
            },
        ),
    ] {
        let options = CaptureOptions {
            start: Some(i32::MIN),
            ..options
        };
        reference.compare(&terminal, flags, options);
        reference.compare(&terminal.snapshot().restore().unwrap(), flags, options);
    }
}

#[test]
fn allocation_sensitive_trailing_capture_survives_empty_resize() {
    let mut terminal = Terminal::new(20, 3, 20);
    terminal.resize(19, 3);
    assert_eq!(
        terminal
            .capture_with(CaptureOptions {
                preserve_trailing: true,
                ..Default::default()
            })
            .unwrap(),
        b"\n\n\n"
    );
}

#[test]
fn snapshot_restores_after_non_reflow_width_shrink() {
    for (input, alternate) in [
        (b"\x1b[?7l01234567890123456789".as_slice(), false),
        (b"\x1b[?1049h01234567890123456789".as_slice(), true),
    ] {
        let mut terminal = Terminal::new(20, 3, 20);
        terminal.feed(input);
        terminal.resize(10, 3);
        let snapshot = terminal.snapshot();
        let screen = if alternate {
            snapshot.alternate.as_ref().unwrap()
        } else {
            &snapshot.primary
        };
        let row = screen.row(0);
        assert_eq!(screen.row_text(row), "0123456789");
        assert_eq!(screen.tmux_capture_bounds(row.id), Some((10, 20)));
        assert_eq!(snapshot.restore().unwrap().capture(), terminal.capture());
        if alternate {
            terminal.feed(b"\x1b[?1049l");
            terminal.snapshot().restore().unwrap();
        }
    }
}

#[test]
fn snapshot_restores_after_reflow_discards_unknown_capture_rows() {
    let mut terminal = Terminal::new(20, 5, 20);
    terminal.feed(b"abcdefghijklmnopqrstuvwxyz");
    terminal.resize(6, 2);
    terminal.feed(b"\x1b[M");
    terminal.resize(5, 1);
    terminal.feed(b"\x1b[2J");
    terminal.resize(18, 2);
    terminal.snapshot().restore().unwrap();
}

#[test]
fn invalid_snapshot_capture_metadata_is_rejected_before_reflow() {
    let mut terminal = Terminal::new(20, 3, 20);
    terminal.feed_at(b"one\r\ntwo\r\nthree\r\nfour", 123456);
    let snapshot = terminal.snapshot();
    let id = snapshot.primary.all_rows().next().unwrap().id;
    let encoded = serde_json::to_value(&snapshot).unwrap();
    for (field, value) in [
        ("tmux_capture_rows", serde_json::json!([[id, [21, 21]]])),
        (
            "tmux_capture_rows",
            serde_json::json!([[id, [1, u64::MAX]]]),
        ),
        ("tmux_capture_rows", serde_json::json!([[999999, [1, 1]]])),
        ("tmux_row_times", serde_json::json!({"999999":123456})),
        ("tmux_row_flags", serde_json::json!({id.to_string():65535})),
    ] {
        let mut invalid = encoded.clone();
        invalid["primary"][field] = value;
        if let Ok(snapshot) = serde_json::from_value::<tmuxx_terminal::Snapshot>(invalid) {
            assert!(snapshot.restore().is_err(), "accepted invalid {field}");
        }
    }
    assert!(snapshot.restore().is_ok());
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn capture_reflow_allocations_preserve_spaces_colors_and_snapshot_bytes() {
    for bytes in [
        b"READY\r\nabcdefghijklmnopq\r\n0123456789012345678901234567890123456789\r\nlast"
            .as_slice(),
        "READY\r\n中中a\rX\r\n\x1b[31mabcdefghijklmnop\x1b[0m\r\n  spaces  \r\nlast".as_bytes(),
        b"READY\r\n\x1b[44mbackground \x1b[0m\r\nabcdef\x1b[2K\rXY\r\nlast".as_slice(),
    ] {
        let (reference, mut terminal) = fixtures(bytes, 20, 4);
        for width in [20, 8, 32, 6, 20] {
            assert!(
                reference
                    .command(&["resize-window", "-x", &width.to_string()])
                    .status
                    .success()
            );
            terminal.resize(width, 4);
            for flags in ["-pN", "-peN", "-peNT", "-peNJ", "-pNL"] {
                let options = CaptureOptions {
                    start: Some(i32::MIN),
                    preserve_trailing: true,
                    escape: flags.contains('e'),
                    trim_unused: flags.contains('T'),
                    join: flags.contains('J'),
                    number_lines: flags.contains('L'),
                    ..Default::default()
                };
                reference.compare(&terminal, flags, options);
                reference.compare(&terminal.snapshot().restore().unwrap(), flags, options);
            }
        }
    }
}

#[test]
fn snapshot_restore_preserves_color_spelling_in_history_and_saved_cursor() {
    let mut terminal = Terminal::new(20, 2, 20);
    terminal.feed(
        b"\x1b[31mANSI\r\n\x1b[38;5;1mPALETTE\r\n\x1b[92mSAVED\x1b7\x1b[34mNOW\x1b[?1049hALT",
    );
    let encoded = serde_json::to_vec(&terminal.snapshot()).unwrap();
    let snapshot: tmuxx_terminal::Snapshot = serde_json::from_slice(&encoded).unwrap();
    let mut restored = snapshot.restore().unwrap();
    let options = CaptureOptions {
        escape: true,
        start: Some(i32::MIN),
        ..Default::default()
    };
    assert_eq!(
        restored.capture_with(options).unwrap(),
        terminal.capture_with(options).unwrap()
    );
    let resume = b"\x1b[?1049l\x1b8RESTORED";
    restored.feed(resume);
    terminal.feed(resume);
    assert_eq!(
        restored.capture_with(options).unwrap(),
        terminal.capture_with(options).unwrap()
    );
}
