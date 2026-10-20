//! Complete shared CLI matrices executed by MSVC against native pipes and real C over SSH.
#![cfg(windows)]
#[path = "interoperability_windows/common.rs"]
mod common;
use common::{Fixture, SIDES, Side};
use std::{
    fs,
    time::{Duration, Instant},
};
use tmuxx_protocol::{Decoder, Event};

fn engine_paste(payload: &[u8], flags: &[&str], bracket: bool, disabled: bool) -> Vec<u8> {
    use tmuxx_core::{ClientContext, Effect, Engine};
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    let execute = |engine: &mut Engine, context: &mut ClientContext, args: &[&str]| {
        let commands = tmuxx_command::parse_argv(args.iter().map(|arg| arg.as_bytes())).unwrap();
        engine.execute(&commands[0], context).unwrap()
    };
    execute(
        &mut engine,
        &mut context,
        &["new-session", "-d", "-s", "test", ""],
    );
    engine.feed(
        0,
        if bracket {
            b"\x1b[?2004h"
        } else {
            b"\x1b[?2004l"
        },
    );
    engine
        .store_buffer(Some(b"paste".to_vec()), payload.to_vec())
        .unwrap();
    if disabled {
        execute(
            &mut engine,
            &mut context,
            &["select-pane", "-d", "-t", "%0"],
        );
    }
    let args = [
        vec!["paste-buffer", "-b", "paste", "-t", "%0"],
        flags.to_vec(),
    ]
    .concat();
    let outcome = execute(&mut engine, &mut context, &args);
    if flags.iter().any(|flag| flag.contains('d')) {
        assert!(!engine.buffers.contains_key("paste"));
    } else {
        assert_eq!(engine.buffers["paste"], payload);
    }
    outcome
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
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn paste_bytes_sanitizing_separator_brackets_and_deletion_match_c() {
    let fixture = Fixture::start();
    for side in SIDES {
        fixture.put(
            side,
            "interop_paste.py",
            include_bytes!("../../../conformance/interop_paste.py"),
        );
    }
    let payload = [
        (0..=255).collect::<Vec<u8>>(),
        "é日本語\u{80}\n".as_bytes().to_vec(),
        b"\xc0\xaf\xed\xa0\x80\xf4\x90\x80\x80".to_vec(),
    ]
    .concat();
    let native_payload = "Hello\né日本語\tend".as_bytes();
    for (case, (flags, bracket, disabled)) in [
        (vec![], false, false),
        (vec!["-S"], false, false),
        (vec!["-r"], false, false),
        (vec!["-r", "-s", "|"], false, false),
        (vec!["-s", ""], false, false),
        (vec!["-s", "\x1b[31m"], false, false),
        (vec!["-p"], false, false),
        (vec!["-p"], true, false),
        (vec!["-pSd"], true, false),
        (vec!["-pd"], true, true),
    ]
    .into_iter()
    .enumerate()
    {
        for side in SIDES {
            let bytes = if side == Side::Reference {
                payload.as_slice()
            } else {
                native_payload
            };
            let leaf = format!("paste-{case}");
            let path = fixture.path(side, &leaf);
            let result = fixture.run(
                side,
                &[
                    "new-window",
                    "-d",
                    "-t",
                    "test:9",
                    fixture.python(side),
                    "-u",
                    &fixture.path(side, "interop_paste.py"),
                    &path,
                    if bracket { "1" } else { "0" },
                ],
                None,
            );
            assert!(result.status.success(), "{side:?}: {result:?}");
            let deadline = Instant::now() + Duration::from_secs(15);
            while !fixture.exists(side, &format!("{leaf}.ready")) {
                assert!(Instant::now() < deadline, "{side:?}: paste child not ready");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                fixture
                    .run(side, &["load-buffer", "-b", "paste", "-"], Some(bytes))
                    .status
                    .success()
            );
            if disabled {
                assert!(
                    fixture
                        .run(side, &["select-pane", "-d", "-t", "test:9.0"], None)
                        .status
                        .success()
                );
            }
            let args = [
                vec!["paste-buffer", "-b", "paste", "-t", "test:9.0"],
                flags.clone(),
            ]
            .concat();
            assert!(fixture.run(side, &args, None).status.success());
            let saved = fixture.run(side, &["show-buffer", "-b", "paste"], None);
            if flags.iter().any(|flag| flag.contains('d')) {
                assert!(!saved.status.success());
                assert_eq!(saved.stderr, b"no buffer paste\n");
            } else {
                assert!(saved.status.success());
                assert_eq!(saved.stdout, bytes);
            }
            if disabled {
                assert!(
                    fixture
                        .run(side, &["select-pane", "-e", "-t", "test:9.0"], None)
                        .status
                        .success()
                );
            }
            assert!(
                fixture
                    .run(
                        side,
                        &[
                            "send-keys",
                            "-l",
                            "-t",
                            "test:9.0",
                            "tmuxx-buffer-end-marker"
                        ],
                        None
                    )
                    .status
                    .success()
            );
            while !fixture.exists(side, &leaf) {
                assert!(
                    Instant::now() < deadline,
                    "{side:?}: paste child did not receive marker"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                fixture
                    .run(side, &["kill-window", "-t", "test:9"], None)
                    .status
                    .success()
            );
            let observed = fixture.get(side, &leaf);
            let expected = engine_paste(bytes, &flags, bracket, disabled);
            assert_eq!(observed, expected, "{side:?}: case{case} flags{flags:?}");
            if side == Side::Reference {
                if disabled {
                    assert!(observed.is_empty());
                }
                if flags == ["-S"] {
                    assert_eq!(
                        observed,
                        payload
                            .iter()
                            .map(|b| if *b == b'\n' { b'\r' } else { *b })
                            .collect::<Vec<_>>()
                    );
                }
                if bracket && !disabled {
                    assert!(observed.starts_with(b"\x1b[200~") && observed.ends_with(b"\x1b[201~"));
                }
            }
        }
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn buffer_files_expand_in_queue_and_preserve_validation_and_io_errors() {
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for side in SIDES {
        fixture.put(side, "formatted-input", b"formatted\0\xff\n");
        let input = fixture.path(side, "formatted-input");
        let output = fixture.path(side, "formatted-output");
        let missing = format!("{}/file", fixture.path(side, "no-such-parent"));
        let null = if side == Side::Native {
            "NUL"
        } else {
            "/dev/null"
        };
        let null_format = format!("#{{l:{null}}}");
        let mut outputs = Vec::new();
        for (index, args) in [
            vec![
                "set", "-g", "@file", &input, ";", "loadb", "-b", "file", "#{@file}", ";", "saveb",
                "-b", "file", "-",
            ],
            vec![
                "set", "-g", "@file", &output, ";", "saveb", "-b", "file", "#{@file}",
            ],
            vec!["saveb", "-a", "-b", "file", "#{@file}"],
            vec!["loadb", "-b", "file", "#{@file}"],
            vec!["saveb", "-b", "file", "#{l:-}"],
            vec!["loadb", "-b", "file", &null_format],
            vec!["saveb", "-b", "file", "-"],
            vec!["saveb", "-b", "missing", &missing],
            vec!["saveb", "-b", "file", &missing],
            vec!["loadb", "-b", "", &missing],
            vec!["loadb", "-b", "", &input],
            vec!["loadb", "-z", &missing],
        ]
        .into_iter()
        .enumerate()
        {
            let result = fixture.run(side, &args, None);
            if [8, 9].contains(&index) {
                assert!(!result.status.success(), "{side:?}: {args:?}");
                assert!(result.stdout.is_empty());
                let expected = format!("No such file or directory: {missing}\n");
                assert_eq!(result.stderr, expected.as_bytes(), "{side:?}: {args:?}");
            } else {
                outputs.push((result.status.code(), result.stdout, result.stderr));
            }
        }
        assert_eq!(
            fixture.get(side, "formatted-output"),
            b"formatted\0\xff\nformatted\0\xff\n"
        );
        assert!(!fixture.exists(side, "no-such-parent"));
        observations.push(outputs);
    }
    assert_eq!(observations[0], observations[1]);
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn same_user_acl_lists_owner_and_rejects_changes_without_mutation() {
    let fixture = Fixture::start();
    let list = |side| fixture.run(side, &["server-access", "-l"], None);
    let c = list(Side::Reference);
    let native = list(Side::Native);
    assert!(c.status.success() && native.status.success());
    // Windows owns a SID account rather than the oracle's Unix UID account.
    let mut username = [0u16; 256];
    let mut count = username.len() as u32;
    assert_ne!(
        unsafe {
            windows_sys::Win32::System::WindowsProgramming::GetUserNameW(
                username.as_mut_ptr(),
                &mut count,
            )
        },
        0
    );
    let owner = String::from_utf16(&username[..count as usize - 1]).unwrap();
    assert_eq!(native.stdout, format!("{owner} (U,W)\n").as_bytes());
    assert!(c.stdout.ends_with(b" (U,W)\n"));
    let graph = [
        "list-panes",
        "-aF",
        "#{pane_id}|#{pane_pid}|#{pane_width}|#{pane_height}",
    ];
    let before = fixture.run(Side::Native, &graph, None).stdout;
    for flags in [
        vec!["-a"],
        vec!["-r"],
        vec!["-w"],
        vec!["-d"],
        vec!["-ad"],
        vec!["-rw"],
        vec!["-ag"],
        vec![],
    ] {
        let mut args = vec!["server-access"];
        args.extend(flags);
        args.push("nobody");
        let output = fixture.run(Side::Native, &args, None);
        assert!(!output.status.success(), "{args:?}");
        assert_eq!(
            output.stderr,
            b"server-access changes are unsupported: server is same-user-only\n"
        );
        assert_eq!(list(Side::Native).stdout, native.stdout);
        assert_eq!(fixture.run(Side::Native, &graph, None).stdout, before);
        // Reopen and authenticate the same endpoint after every rejected ACL mutation.
        let mut pipe = tmuxx_runtime::windows::connect(&fixture.endpoint).unwrap();
        tmuxx_runtime::native_handshake(&mut pipe).unwrap();
    }
    assert!(
        fixture
            .run(Side::Reference, &["server-access", "-a", "nobody"], None)
            .status
            .success()
    );
    assert!(
        list(Side::Reference)
            .stdout
            .windows(b"nobody (U,W)".len())
            .any(|row| row == b"nobody (U,W)")
    );
    assert!(
        fixture
            .run(Side::Reference, &["server-access", "-d", "nobody"], None)
            .status
            .success()
    );
    assert_eq!(list(Side::Reference).stdout, c.stdout);
    for side in SIDES {
        let output = fixture.run(side, &["server-access"], None);
        assert!(!output.status.success());
        assert_eq!(output.stderr, b"missing user or group argument\n");
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn clap_driver_help_and_version_are_explicit_implementation_differences() {
    let fixture = Fixture::start();
    for flag in ["-h", "--help"] {
        let c = fixture.run(Side::Reference, &[flag], None);
        let native = fixture.run(Side::Native, &[flag], None);
        assert_eq!(c.status.success(), flag == "-h");
        assert!(
            [c.stdout.as_slice(), c.stderr.as_slice()]
                .concat()
                .windows(6)
                .any(|part| part == b"usage:")
        );
        assert!(native.status.success());
        assert!(native.stderr.is_empty());
        assert!(
            native
                .stdout
                .windows(b"Headless tmux-compatible".len())
                .any(|part| part == b"Headless tmux-compatible")
        );
    }
    let c = fixture.run(Side::Reference, &["-V"], None);
    let native = fixture.run(Side::Native, &["-V"], None);
    assert!(c.status.success() && native.status.success());
    assert!(c.stdout.starts_with(b"tmux "));
    assert_eq!(
        native.stdout,
        format!("tmuxx {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn load_buffer_limit_is_explicit_preserves_buffers_and_recovers_file_streams() {
    let fixture = Fixture::start();
    let limit = tmuxx_runtime::MAX_REQUEST_BYTES;
    for side in SIDES {
        fixture.put(side, "buffer-limit", &vec![0; limit]);
        fixture.put(side, "buffer-excessive", &vec![0; limit + 1]);
    }
    let boundary = fixture.path(Side::Native, "buffer-limit");
    let excessive = fixture.path(Side::Native, "buffer-excessive");
    let local = |args: &[&str]| {
        raw_run(
            &fixture,
            Side::Native,
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            None,
        )
    };
    assert!(
        local(&["load-buffer", "-b", "native-boundary", &boundary])
            .status
            .success()
    );
    assert_eq!(
        local(&[
            "list-buffers",
            "-f",
            "#{==:#{buffer_name},native-boundary}",
            "-F",
            "#{buffer_size}"
        ])
        .stdout,
        format!("{limit}\n").as_bytes()
    );
    assert!(
        local(&["set-buffer", "-b", "native-kept", "original"])
            .status
            .success()
    );
    let rejected = local(&[
        "load-buffer",
        "-b",
        "native-kept",
        &excessive,
        ";",
        "display-message",
        "-p",
        "AFTER",
    ]);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert_eq!(rejected.stderr, b"buffer exceeds input size limit\n");
    assert_eq!(
        local(&["show-buffer", "-b", "native-kept"]).stdout,
        b"original"
    );
    for side in SIDES {
        let boundary = fixture.path(side, "buffer-limit");
        let excessive = fixture.path(side, "buffer-excessive");
        let run = |args: &[&str]| fixture.run(side, args, None);
        assert!(
            run(&["load-buffer", "-b", "boundary", &boundary])
                .status
                .success()
        );
        assert_eq!(
            run(&[
                "list-buffers",
                "-f",
                "#{==:#{buffer_name},boundary}",
                "-F",
                "#{buffer_size}"
            ])
            .stdout,
            format!("{limit}\n").as_bytes()
        );
        assert!(
            run(&["set-buffer", "-b", "kept", "original"])
                .status
                .success()
        );
        let result = run(&[
            "load-buffer",
            "-b",
            "kept",
            &excessive,
            ";",
            "display-message",
            "-p",
            "AFTER",
        ]);
        if side == Side::Native {
            assert!(!result.status.success());
            assert!(result.stdout.is_empty());
            assert_eq!(result.stderr, b"buffer exceeds input size limit\n");
            assert_eq!(run(&["show-buffer", "-b", "kept"]).stdout, b"original");
        } else {
            assert!(result.status.success());
            assert_eq!(result.stdout, b"AFTER\n");
            assert_eq!(
                run(&[
                    "list-buffers",
                    "-f",
                    "#{==:#{buffer_name},kept}",
                    "-F",
                    "#{buffer_size}"
                ])
                .stdout,
                format!("{}\n", limit + 1).as_bytes()
            );
        }
        assert_eq!(
            run(&["display-message", "-p", "RECOVERED"]).stdout,
            b"RECOVERED\n"
        );
        assert!(
            run(&["set-buffer", "-b", "kept", "original"])
                .status
                .success()
        );
        let input = vec![b'z'; limit + 1];
        let result = fixture.run(side, &["load-buffer", "-b", "kept", "-"], Some(&input));
        if side == Side::Native {
            assert!(!result.status.success());
            assert_eq!(result.stderr, b"file input exceeds limit: -\n");
            assert_eq!(run(&["show-buffer", "-b", "kept"]).stdout, b"original");
        } else {
            assert!(result.status.success());
            assert_eq!(
                run(&[
                    "list-buffers",
                    "-f",
                    "#{==:#{buffer_name},kept}",
                    "-F",
                    "#{buffer_size}"
                ])
                .stdout,
                format!("{}\n", limit + 1).as_bytes()
            );
        }
    }
    // Native control queues retain the C deferred-file guard and recovery contract.
    let mut control = fixture.control(Side::Native);
    let records = control.records(&format!(
        "load-buffer -b kept '{}'\ndisplay-message -p CONTROL-RECOVERED",
        excessive
    ));
    let errors: Vec<_> = records
        .iter()
        .filter_map(|event| {
            if let Event::UnframedOutput(line) = event {
                Some(line)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        errors,
        [&format!("file input exceeds limit: {excessive}").into_bytes()]
    );
    assert!(
        records
            .iter()
            .any(|event| matches!(event,Event::CommandOutput(line) if line==b"CONTROL-RECOVERED"))
    );
    assert!(
        records
            .iter()
            .all(|event| !matches!(event, Event::End { failed: true, .. }))
    );
    assert_eq!(
        fs::metadata(fixture.directory.join("buffer-limit"))
            .unwrap()
            .len(),
        limit as u64
    );
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn command_control_files_and_origin_context_match_c_over_native_pipes() {
    let fixture = Fixture::start();
    for side in SIDES {
        let run = |args: &[&str], input| fixture.run(side, args, input);
        assert_eq!(
            run(&["display-message", "-p", "wire \\ bytes"], None).stdout,
            b"wire \\ bytes\n"
        );
        let failure = run(&["select-pane", "-t", "%99999999"], None);
        assert!(!failure.status.success());
        assert!(!failure.stderr.is_empty());
        let partial = run(
            &[
                "display-message",
                "-p",
                "BEFORE",
                ";",
                "select-pane",
                "-t",
                "%99999999",
                ";",
                "display-message",
                "-p",
                "AFTER",
            ],
            None,
        );
        assert!(!partial.status.success());
        assert_eq!(partial.stdout, b"BEFORE\n");
        assert!(
            run(&["load-buffer", "-"], Some(b"binary\0\xff\n"))
                .status
                .success()
        );
        assert_eq!(run(&["save-buffer", "-"], None).stdout, b"binary\0\xff\n");
        fixture.put(side, "input.buffer", b"file\0\xff\n");
        let input = fixture.path(side, "input.buffer");
        let output = fixture.path(side, "output.buffer");
        let control_file = fixture.path(side, "control.buffer");
        assert!(run(&["load-buffer", &input], None).status.success());
        assert!(run(&["save-buffer", &output], None).status.success());
        assert!(run(&["save-buffer", "-a", &output], None).status.success());
        assert_eq!(
            fixture.get(side, "output.buffer"),
            b"file\0\xff\nfile\0\xff\n"
        );
        assert!(
            !run(&["load-buffer", &fixture.path(side, "absent")], None)
                .status
                .success()
        );
        let mut control = fixture.control(side);
        let records=control.records(&format!("display-message -p BEFORE ; select-pane -t %99999999 ; display-message -p AFTER\nload-buffer '{input}'\nsave-buffer '{control_file}'\ndisplay-message -p RECOVERED"));
        let lines: Vec<_> = records
            .iter()
            .filter_map(|e| {
                if let Event::CommandOutput(line) = e {
                    Some(line.as_slice())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            lines.contains(&b"BEFORE".as_slice()) && lines.contains(&b"RECOVERED".as_slice()),
            "{side:?}: {records:?}"
        );
        assert!(!lines.contains(&b"AFTER".as_slice()));
        let outcomes: Vec<_> = records
            .iter()
            .filter_map(|e| {
                if let Event::End { failed, .. } = e {
                    Some(*failed)
                } else {
                    None
                }
            })
            .collect();
        let error = outcomes.iter().position(|failed| *failed).unwrap();
        assert!(outcomes[error + 1..].iter().any(|failed| !*failed));
        assert_eq!(fixture.get(side, "control.buffer"), b"file\0\xff\n");
        assert!(run(&["has-session", "-t", "test"], None).status.success());
        drop(control);
        let program = "import os,time;print(os.getcwd(),flush=True);print(os.environ['DISPLAY'],flush=True);time.sleep(30)";
        let args = [
            "new-session",
            "-d",
            "-s",
            "origin",
            "-x",
            "240",
            fixture.python(side),
            "-u",
            "-c",
            program,
        ];
        let result = if side == Side::Reference {
            fixture.reference.output_with_environment(
                &args.iter().map(|s| s.as_bytes()).collect::<Vec<_>>(),
                b"",
                &[(b"DISPLAY", Some(b"tmuxx-origin-marker"))],
            )
        } else {
            fixture
                .native_command()
                .args(args)
                .env("DISPLAY", "tmuxx-origin-marker")
                .output()
                .unwrap()
        };
        assert!(result.status.success(), "{side:?}: {result:?}");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let capture = run(&["capture-pane", "-pJ", "-t", "origin:0"], None);
            assert!(capture.status.success());
            let text = String::from_utf8(capture.stdout).unwrap();
            if text.contains("tmuxx-origin-marker") {
                assert!(text.contains(&fixture.cwd(side)), "{side:?}: {text}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{side:?}: origin context not propagated: {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            run(&["kill-session", "-t", "origin"], None)
                .status
                .success()
        );
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn native_and_unix_control_preserve_c_command_guards_blocks_and_file_callbacks() {
    let fixture = Fixture::start();
    let mut c = fixture.control(Side::Reference);
    let mut native = fixture.control(Side::Native);
    c.records("display -p ready");
    native.records("display -p ready");
    for command in [
        "setenv -g CONTROL_PREP parsed",
        "if-shell -F 1 { setenv -g CONTROL_PREP changed; display -p \"block:$CONTROL_PREP\"; if-shell -F 1 'display -p \"string:$CONTROL_PREP\"' }",
        "set -s command-alias[90] 'late=display-message -p original'",
        "if-shell -F 1 { set -s command-alias[90] 'late=display-message -p changed'; if-shell -F 1 { late }; if-shell -F 1 'late' }",
        "run-shell -C -d 0.01 { set -g @value after; display -p 'block:#{@value}:#{command}' }",
        "if-shell 'exit 0' { display -p async-if }",
        "set-hook -g after-rename-window { run-shell -C { display -p hook } }",
        "rename-window -t test:0 renamed",
        "set-hook -gu after-rename-window",
        "if-shell -F 0 { no-such-command }",
        "source-file -",
        "load-buffer -b stdin -; display -p after-stdin-error",
    ] {
        assert_eq!(native.records(command), c.records(command), "{command}");
    }
    let commands=SIDES.map(|side|format!("run-shell '\"{}\" -c \"import sys;sys.stdout.buffer.write(b\\\"job\\\");sys.exit(3)\"'",fixture.python(side)));
    let mut expected = c.records(&commands[0]);
    for event in &mut expected {
        if let Event::UnframedOutput(line) = event {
            *line = String::from_utf8(line.clone())
                .unwrap()
                .replace(
                    fixture.python(Side::Reference),
                    fixture.python(Side::Native),
                )
                .into_bytes();
        }
    }
    assert_eq!(native.records(&commands[1]), expected);
    let paths = SIDES.map(|side| fixture.path(side, "control.conf"));
    // Replace only known private fixture paths and exact host OS diagnostics;
    // all guards, command output and record ordering stay byte-for-byte visible.
    let native_expected = |events: Vec<Event>| {
        events
            .into_iter()
            .map(|mut event| {
                if let Event::UnframedOutput(line)
                | Event::CommandOutput(line)
                | Event::Notification(tmuxx_protocol::Notification::ConfigError(line)) =
                    &mut event
                {
                    let mut text = String::from_utf8(line.clone()).unwrap();
                    for leaf in [
                        "control.conf",
                        "absent-control-source",
                        "absent-control-buffer",
                        "control-buffer",
                    ] {
                        text = text.replace(
                            &fixture.path(Side::Reference, leaf),
                            &fixture.path(Side::Native, leaf),
                        );
                    }
                    *line = text.into_bytes();
                }
                event
            })
            .collect::<Vec<_>>()
    };
    for source in [
        "display -p first\nif-shell -F 1 { display -p nested }\ndisplay -p last\n",
        "not-a-command\n",
        "display -p first; source-file absent-control-source; display -p following\n",
    ] {
        for side in SIDES {
            fixture.put(side, "control.conf", source.as_bytes());
        }
        assert_eq!(
            native.records(&format!("source-file '{}'", paths[1])),
            native_expected(c.records(&format!("source-file '{}'", paths[0]))),
            "{source}"
        );
    }
    for side in SIDES {
        fixture.put(side, "control.conf", b"display -p surviving-file\n");
        fixture.put(side, "control-buffer", b"buffer line\n");
    }
    assert_eq!(
        native.records(&format!(
            "source-file absent-control-source '{}' ; display -p after-source",
            paths[1]
        )),
        native_expected(c.records(&format!(
            "source-file absent-control-source '{}' ; display -p after-source",
            paths[0]
        )))
    );
    let commands = SIDES.map(|side| {
        format!(
            "load-buffer -b missing '{}'; display -p after-file-error",
            fixture.path(side, "absent-control-buffer")
        )
    });
    assert_eq!(
        native.records(&commands[1]),
        native_expected(c.records(&commands[0]))
    );
    let commands = SIDES.map(|side| {
        format!(
            "load-buffer -b loaded '{}'",
            fixture.path(side, "control-buffer")
        )
    });
    assert_eq!(native.records(&commands[1]), c.records(&commands[0]));
    for command in ["save-buffer -b loaded -", "delete-buffer -b loaded"] {
        assert_eq!(native.records(command), c.records(command), "{command}");
    }
    let background =
        "run-shell -b -C -d 0.01 { set -g @background done; display -p background-done }";
    assert_eq!(
        native.records_until(background, "background-done", false),
        c.records_until(background, "background-done", false)
    );
    for (side, control) in [(Side::Reference, &mut c), (Side::Native, &mut native)] {
        control.write(b"\n");
        assert!(
            matches!(
                control.next(),
                Event::Notification(tmuxx_protocol::Notification::Exit { .. })
            ),
            "{side:?}"
        );
    }
}

fn raw_run(
    fixture: &Fixture,
    side: Side,
    args: &[&[u8]],
    input: Option<&[u8]>,
) -> std::process::Output {
    if side == Side::Reference {
        return fixture.reference.output(args, input.unwrap_or_default());
    }
    use std::os::windows::process::ExitStatusExt;
    use tmuxx_protocol::native::{Request, ServerMessage};
    let local = fixture.host.as_ref().unwrap().connect().unwrap();
    local
        .send(Request {
            request_id: 1,
            argv: args.iter().map(|arg| arg.to_vec()).collect(),
            stdin: input.map(<[u8]>::to_vec),
            subscribe: false,
            control: false,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = local.try_recv() {
            return std::process::Output {
                status: std::process::ExitStatus::from_raw(
                    reply.status.unwrap_or(i32::from(reply.error.is_some())) as u32,
                ),
                stdout: reply.output,
                stderr: reply
                    .error
                    .map(|error| format!("{error}\n").into_bytes())
                    .unwrap_or_default(),
            };
        }
        assert!(Instant::now() < deadline, "native command reply deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn cli_preserves_non_nul_argv_command_diagnostics_status_and_features() {
    use std::os::windows::ffi::OsStringExt;
    let fixture = Fixture::start();
    // Native protocol retains all bytes; CreateProcess argv itself is UTF-16.
    let bytes: Vec<u8> = (1..=255).collect();
    for side in SIDES {
        let output = raw_run(&fixture, side, &[b"display", b"-pl", &bytes], None);
        assert!(output.status.success(), "{side:?}: {output:?}");
        assert_eq!(output.stdout, [bytes.as_slice(), b"\n"].concat());
        assert!(output.stderr.is_empty());
        for text in [
            "",
            "é日本語 Ω",
            "a & | ^ < > % ! ; $ ` ' \\\"",
            "line\n\ttab",
        ] {
            let result = fixture.run(side, &["display", "-pl", text], None);
            assert!(result.status.success(), "{side:?}: {result:?}");
            assert_eq!(result.stdout, format!("{text}\n").as_bytes());
            assert!(result.stderr.is_empty());
        }
    }
    for units in [
        &[0xd800][..],
        &[0xdc00],
        &[b'x' as u16, 0xd800, b'y' as u16],
    ] {
        let result = fixture
            .native_command()
            .args(["display", "-pl"])
            .arg(std::ffi::OsString::from_wide(units))
            .output()
            .unwrap();
        assert!(!result.status.success(), "unpaired UTF-16 was accepted");
        assert!(result.stdout.is_empty());
        assert!(!result.stderr.windows(3).any(|part| part == "�".as_bytes()));
    }
    for args in [
        vec!["display", "-p", ""],
        vec!["display", "-p", "-F", "x", "y"],
        vec!["list-c"],
        vec!["ls", "-z"],
        vec!["run-shell", "exit 7"],
        vec!["list-commands", "-h", "new"],
    ] {
        let c = fixture.run(Side::Reference, &args, None);
        let native = fixture.run(Side::Native, &args, None);
        assert_eq!(
            (native.status.code(), native.stdout, native.stderr),
            (c.status.code(), c.stdout, c.stderr),
            "{args:?}"
        );
    }
    for features in [
        vec!["-2", "-TRGB,clipboard", "-u", "-q"],
        vec!["-T256,rgb", "-T256@", "-2"],
        vec!["-2", "-T256@,focus", "-Trgb:unknown:mouse"],
    ] {
        let mut values = Vec::new();
        for side in SIDES {
            let args = [features.clone(), vec!["-C", "attach-session", "-t", "test"]].concat();
            let mut pending = fixture.spawn(side, &args);
            pending.write(b"display -p '#{client_termfeatures}|#{client_utf8}'\ndetach\n");
            pending.finish_input();
            let output = pending.output();
            assert!(output.status.success(), "{side:?}: {output:?}");
            let replies = Decoder::default().feed(&output.stdout).unwrap();
            values.push(
                replies
                    .into_iter()
                    .find_map(|event| match event {
                        Event::CommandOutput(output) if output.contains(&b'|') => Some(output),
                        _ => None,
                    })
                    .expect("feature response"),
            );
        }
        assert_eq!(values[0], values[1], "{features:?}");
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn invalid_buffer_names_are_rejected_with_safe_native_diagnostic_difference() {
    let fixture = Fixture::start();
    for side in SIDES {
        let run = |args: &[&[u8]], stdin| raw_run(&fixture, side, args, stdin);
        for (name, value) in [
            (b"kept".as_slice(), b"old".as_slice()),
            ("�".as_bytes(), b"valid Unicode"),
        ] {
            assert!(
                run(&[b"set-buffer", b"-b", name, value], None)
                    .status
                    .success()
            );
        }
        let before = run(
            &[
                b"list-buffers",
                b"-F",
                b"#{buffer_name}|#{buffer_size}|#{buffer_sample}",
            ],
            None,
        )
        .stdout;
        for (args, prefix) in [
            (
                vec![b"show-buffer".as_slice(), b"-b", b"\xff"],
                b"no buffer ".as_slice(),
            ),
            (vec![b"delete-buffer", b"-b", b"\xff"], b"unknown buffer: "),
            (
                vec![b"set-buffer", b"-b", b"\xff", b"-n", b"renamed"],
                b"unknown buffer: ",
            ),
            (vec![b"paste-buffer", b"-db", b"\xff"], b"no buffer "),
            (vec![b"save-buffer", b"-b", b"\xff", b"-"], b"no buffer "),
        ] {
            let output = run(&args, None);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let suffix = if side == Side::Reference {
                b"\xff\n".as_slice()
            } else {
                b"\\xff\n"
            };
            assert_eq!(output.stderr, [prefix, suffix].concat());
            assert_eq!(
                run(
                    &[
                        b"list-buffers",
                        b"-F",
                        b"#{buffer_name}|#{buffer_size}|#{buffer_sample}"
                    ],
                    None
                )
                .stdout,
                before
            );
        }
        for name in [
            b"bad\r".as_slice(),
            b"bad\x1b",
            b"bad\x7f",
            b"\xff\n%exit",
            b"\xc3",
            b"\xc0\x80",
        ] {
            for args in [
                vec![b"set-buffer".as_slice(), b"-b", name, b"new"],
                vec![b"set-buffer", b"-b", b"kept", b"-n", name],
                vec![b"capture-pane", b"-b", name],
                vec![b"load-buffer", b"-b", name, b"-"],
            ] {
                let output = run(&args, Some(b"new"));
                assert!(!output.status.success(), "{side:?}: {args:?}");
                assert!(output.stdout.is_empty());
                let expected = if side == Side::Reference {
                    [b"invalid buffer name: ".as_slice(), name, b"\n"].concat()
                } else {
                    format!("invalid buffer name: {}\n", name.escape_ascii()).into_bytes()
                };
                assert_eq!(output.stderr, expected, "{side:?}: {args:?}");
                assert_eq!(
                    run(
                        &[
                            b"list-buffers",
                            b"-F",
                            b"#{buffer_name}|#{buffer_size}|#{buffer_sample}"
                        ],
                        None
                    )
                    .stdout,
                    before
                );
            }
            let null = if side == Side::Native {
                b"NUL".as_slice()
            } else {
                b"/dev/null"
            };
            let output = run(&[b"load-buffer", b"-b", name, null], None);
            assert!(output.status.success(), "{side:?}: {output:?}");
        }
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn display_message_uses_control_notification_and_print_flag_like_c() {
    let fixture = Fixture::start();
    for side in SIDES {
        let output = fixture.run(side, &["display-message", "no recipient"], None);
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        let mut control = fixture.control(side);
        let records =
            control.records("display-message 'message body'; display-message -p 'print body'");
        let messages = records
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::Notification(tmuxx_protocol::Notification::Message(_))
                )
            })
            .count();
        let printed: Vec<_> = records
            .iter()
            .filter_map(|event| {
                if let Event::CommandOutput(data) = event {
                    Some(data.as_slice())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(messages, 0);
        assert_eq!(
            printed,
            [
                b"%message message body".as_slice(),
                b"print body",
                b"__native_control_barrier"
            ],
            "{side:?}"
        );
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn buffer_formats_samples_filters_and_creation_times_match_c() {
    let fixture = Fixture::start();
    let samples = [
        b"\0".to_vec(),
        b"a\0b\x07\x08\x0b\x0c\x1b\x7f\\\xff\0".to_vec(),
        "é日本語\\\t\n".as_bytes().to_vec(),
        vec![b'x'; 199],
        vec![b'x'; 200],
        vec![b'x'; 201],
        vec![1; 70],
        [vec![b'x'; 199], "é".as_bytes().to_vec()].concat(),
    ];
    for sample in samples {
        let mut observations = Vec::new();
        for side in SIDES {
            let output = fixture.run(side, &["load-buffer", "-"], Some(&sample));
            assert!(output.status.success(), "{output:?}");
            let mut results = Vec::new();
            for args in [
                vec![
                    "display-message",
                    "-p",
                    "#{buffer_name}|#{buffer_size}|#{buffer_sample}|#{buffer_full}",
                ],
                vec![
                    "list-buffers",
                    "-f",
                    "#{==:#{buffer_name},#{buffer_name}}",
                    "-F",
                    "#{buffer_name}=#{buffer_sample}",
                ],
                vec!["list-buffers", "-f", "0"],
            ] {
                let output = fixture.run(side, &args, None);
                assert!(output.status.success(), "{output:?}");
                results.push(output.stdout);
            }
            observations.push(results);
        }
        assert_eq!(observations[0], observations[1], "sample {sample:?}");
    }
    for side in SIDES {
        let run = |args: &[&str]| fixture.run(side, args, None);
        let query = |name: &str| {
            let result = run(&[
                "list-buffers",
                "-f",
                &format!("#{{==:#{{buffer_name}},{name}}}"),
                "-F",
                "#{buffer_created}",
            ]);
            assert!(result.status.success(), "{result:?}");
            String::from_utf8(result.stdout)
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap()
        };
        assert!(
            run(&["set-buffer", "-b", "created", "value"])
                .status
                .success()
        );
        let created = query("created");
        assert!(created > 0);
        assert!(
            run(&["set-buffer", "-b", "created", "-n", "renamed"])
                .status
                .success()
        );
        assert_eq!(query("renamed"), created);
        std::thread::sleep(Duration::from_millis(1100));
        assert!(
            run(&["set-buffer", "-ab", "renamed", "more"])
                .status
                .success()
        );
        assert!(query("renamed") > created);
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn scheduler_command_formats_are_canonical_and_do_not_leak_to_nested_items() {
    let fixture = Fixture::start();
    for side in SIDES {
        let shell = format!(
            "{} -c \"import sys;sys.stdout.buffer.write(b'#{{command}}\\n')\"",
            fixture.python(side)
        );
        let run = |args: &[&str], input| fixture.run(side, args, input);
        for (args, expected) in [
            (vec!["run", shell.as_str()], b"run-shell\n".as_slice()),
            (
                vec![
                    "if",
                    "-F",
                    "#{==:#{command},if-shell}",
                    "display -p '#{command}'",
                    "display -p wrong",
                ],
                b"display-message\n",
            ),
            (vec!["run", "-C", "display -p '#{command}'"], b"run-shell\n"),
            (vec!["run", "-C", "display -p extra", "ignored"], b"extra\n"),
        ] {
            let output = run(&args, None);
            assert!(output.status.success(), "{args:?}: {output:?}");
            assert_eq!(output.stdout, expected, "{side:?}: {args:?}");
        }
        let output = run(
            &["source", "-"],
            Some(b"display -p '#{command}'\n".as_slice()),
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"display-message\n");
        let output = run(
            &[
                "set-hook",
                "-g",
                "after-new-window",
                "set -gF @hook-command '#{command}'",
            ],
            None,
        );
        assert!(output.status.success(), "{output:?}");
        assert!(
            run(
                &[
                    "new-window",
                    "-d",
                    fixture.python(side),
                    "-c",
                    "import time;time.sleep(30)"
                ],
                None
            )
            .status
            .success()
        );
        assert_eq!(
            run(&["show-options", "-gv", "@hook-command"], None).stdout,
            b"new-window\n"
        );
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn control_buffer_notifications_cover_replacement_eviction_capture_load_and_paste() {
    use tmuxx_protocol::Notification::{PasteBufferChanged, PasteBufferDeleted};
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for side in SIDES {
        let null = if side == Side::Native {
            "NUL"
        } else {
            "/dev/null"
        };
        let mut control = fixture.control(side);
        let mut batches = Vec::new();
        for (args, stdin) in [
            (vec!["capture-pane", "-P"], None),
            (vec!["capture-pane", "-P", "-b", "invalid\x1b"], None),
            (vec!["load-buffer", "-b", "invalid\x1b", null], None),
            (vec!["set-option", "-s", "buffer-limit", "1"], None),
            (vec!["set-buffer", "one"], None),
            (vec!["set-buffer", "two"], None),
            (vec!["set-buffer", "-b", "named", "first"], None),
            (vec!["set-buffer", "-ab", "named", "second"], None),
            (vec!["set-buffer", "-b", "named", "-n", "renamed"], None),
            (vec!["set-buffer", "-b", "line\n\t\\é", "first"], None),
            (
                vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "line\n\t\\é"],
                None,
            ),
            (vec!["set-buffer", "-b", "target\n", "replaced"], None),
            (
                vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "target\n"],
                None,
            ),
            (vec!["load-buffer", "-"], Some(b"from file\0".as_slice())),
            (
                vec!["load-buffer", "-b", "load\n\té", "-"],
                Some(b"named file\0".as_slice()),
            ),
            (vec!["capture-pane", "-t", "test:0.0"], None),
            (
                vec!["capture-pane", "-b", "renamed", "-t", "test:0.0"],
                None,
            ),
            (
                vec!["capture-pane", "-b", "capture\n\t\\", "-t", "test:0.0"],
                None,
            ),
            (
                vec!["paste-buffer", "-d", "-b", "renamed", "-t", "test:0.0"],
                None,
            ),
            (vec!["delete-buffer"], None),
        ] {
            let result = fixture.run(side, &args, stdin);
            assert!(result.status.success(), "{args:?}: {result:?}");
            control.write(b"display-message -p buffer-barrier\n");
            let mut records = Vec::new();
            let mut barrier = false;
            loop {
                match control.next() {
                    Event::Notification(
                        record @ (PasteBufferChanged { .. } | PasteBufferDeleted { .. }),
                    ) => records.push(record),
                    Event::CommandOutput(line) => {
                        assert_eq!(line, b"buffer-barrier");
                        barrier = true;
                    }
                    Event::End { failed, .. } if barrier => {
                        assert!(!failed);
                        break;
                    }
                    _ => {}
                }
            }
            batches.push(records);
        }
        observations.push(batches);
    }
    assert_eq!(observations[0], observations[1]);
    assert!(observations[0][..3].iter().all(Vec::is_empty));
    assert!(
        observations[0]
            .iter()
            .flatten()
            .any(|record| matches!(record, PasteBufferDeleted { .. }))
    );
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn buffer_names_creation_lookup_rename_and_empty_input_match_c() {
    let fixture = Fixture::start();

    let mut observations = Vec::new();
    for side in SIDES {
        let null = if side == Side::Native {
            "NUL"
        } else {
            "/dev/null"
        };
        let mut outputs = Vec::new();
        for args in [
            vec!["capture-pane", "-P"],
            vec!["capture-pane", "-P", "-b", "invalid\x1b"],
            vec!["set-buffer", "-b", "invalid\x1b", ""],
            vec!["load-buffer", "-b", "invalid\x1b", null],
            vec!["set-buffer", "automatic"],
            vec!["set-buffer", "-b", "line\n\t\\é", "first"],
            vec!["show-buffer", "-b", "line\n\t\\é"],
            vec!["show-buffer", "-b", "line\\n\\t\\\\é"],
            vec!["set-buffer", "-ab", "line\n\t\\é", "replacement"],
            vec!["set-buffer", "-ab", "line\\n\\t\\\\é", " appended"],
            vec!["show-buffer", "-b", "line\\n\\t\\\\é"],
            vec!["show-buffer", "-b", "line\\\\n\\\\t\\\\\\\\é"],
            vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "line\n\t\\é"],
            vec!["set-buffer", "-b", "target\n", "old"],
            vec!["set-buffer", "-b", "line\\n\\t\\\\é", "-n", "target\n"],
            vec!["capture-pane", "-b", "capture\n\t\\"],
            vec![
                "list-buffers",
                "-F",
                "#{buffer_name}|#{buffer_size}|#{buffer_sample}",
            ],
        ] {
            let output = fixture.run(side, &args, None);
            outputs.push((output.status.code(), output.stdout, output.stderr));
        }
        observations.push(outputs);
    }
    assert_eq!(observations[0], observations[1], "native and real C");
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn server_diagnostics_expose_native_logs_jobs_limits_and_c_empty_tty_state() {
    let fixture = Fixture::start();
    for side in SIDES {
        let job = format!("{} -c \"import time;time.sleep(30)\"", fixture.python(side));
        let run = |args: &[&str]| fixture.run(side, args, None);
        for flags in ["-T", "-J", "-JT"] {
            let result = run(&["show-messages", flags]);
            assert!(result.status.success(), "{result:?}");
            assert!(result.stdout.is_empty());
        }
        let missing = run(&["show-messages", "-JT", "-t", "not-a-client"]);
        assert!(missing.status.success());
        assert!(missing.stdout.is_empty() && missing.stderr.is_empty());
        assert!(run(&["run-shell", "-b", &job]).status.success());
        let jobs = run(&["show-messages", "-JT"]);
        assert!(jobs.status.success(), "{jobs:?}");
        assert!(jobs.stdout.starts_with(b"Job 0: "));
        assert!(
            jobs.stdout
                .windows(b"time.sleep(30)".len())
                .any(|part| part == b"time.sleep(30)"),
            "{side:?}: {jobs:?}"
        );
        if side == Side::Native {
            assert!(jobs.stdout.ends_with(b"kind=shell]\n"));
            let long_job = format!("{job} {}", "\t".repeat(5000));
            assert!(run(&["run-shell", "-b", &long_job]).status.success());
            let summaries = run(&["show-messages", "-J"]);
            assert!(summaries.status.success());
            let long = summaries
                .stdout
                .split(|byte| *byte == b'\n')
                .find(|line| {
                    line.windows(b"[truncated]".len())
                        .any(|part| part == b"[truncated]")
                })
                .unwrap();
            assert!(long.len() < 4200);
            assert!(long.ends_with(b"kind=shell]"));
        } else {
            assert!(
                jobs.stdout
                    .windows(b"[fd=".len())
                    .any(|part| part == b"[fd=")
            );
        }
        let messages = run(&["show-messages"]);
        assert!(messages.status.success());
        assert!(!messages.stdout.is_empty());
        assert!(run(&["set", "-s", "message-limit", "0"]).status.success());
        assert!(
            run(&["display-message", "-p", "discarded-log"])
                .status
                .success()
        );
        assert!(run(&["show-messages"]).stdout.is_empty());
        assert!(run(&["set", "-s", "message-limit", "2"]).status.success());
        for text in ["log-first", "log-second", "log-third"] {
            assert!(run(&["display-message", "-p", text]).status.success());
        }
        let messages = run(&["show-messages"]);
        assert!(messages.status.success());
        assert!(
            messages
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count()
                <= 2
        );
        if side == Side::Native {
            let lines = String::from_utf8(messages.stdout)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(lines.len(), 2);
            assert!(lines[0].contains("log-third") && lines[1].contains("log-second"));
            assert!(!run(&["select-pane", "-t", "%999999999"]).status.success());
            assert!(
                String::from_utf8(run(&["show-messages"]).stdout)
                    .unwrap()
                    .contains("error:")
            );
            let long = "x".repeat(5_000);
            assert!(run(&["display-message", "-p", &long]).status.success());
            let messages = run(&["show-messages"]);
            assert!(messages.stdout.len() < 9000);
            assert!(
                messages
                    .stdout
                    .windows(b"[truncated]".len())
                    .any(|part| part == b"[truncated]")
            );
        }
        let limit = run(&["set", "-s", "message-limit", "16385"]);
        if side == Side::Native {
            assert!(!limit.status.success());
            assert_eq!(limit.stderr, b"message-limit exceeds native limit: 16384\n");
            assert_eq!(run(&["show", "-sv", "message-limit"]).stdout, b"2\n");
        } else {
            assert!(limit.status.success());
        }
    }
}

#[test]
#[ignore = "requires explicit real C SSH oracle and bundled ConPTY"]
fn display_all_formats_enumerates_present_callbacks_and_native_trace_is_bounded() {
    let fixture = Fixture::start();
    for side in SIDES {
        let script = "import os,time;os.write(1,b'\\x1b[?1004h\\x1b]2;format-fixture\\x07READY');time.sleep(120)";
        assert!(
            fixture
                .run(
                    side,
                    &[
                        "respawn-pane",
                        "-k",
                        "-t",
                        "test:0.0",
                        fixture.python(side),
                        "-c",
                        script
                    ],
                    None
                )
                .status
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let capture = fixture.run(side, &["capture-pane", "-p", "-t", "test:0.0"], None);
            if capture.stdout.starts_with(b"READY") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{side:?}: format fixture output: {capture:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            fixture
                .run(side, &["rename-window", "-t", "test", "stable"], None)
                .status
                .success()
        );
    }
    let maps: Vec<_> = SIDES
        .into_iter()
        .map(|side| {
            let output = fixture.run(side, &["display-message", "-a", "-t", "test"], None);
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| {
                    let (key, value) = line.split_once('=').unwrap();
                    (key.to_owned(), value.to_owned())
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .collect();
    assert_eq!(
        maps[0].keys().collect::<Vec<_>>(),
        maps[1].keys().collect::<Vec<_>>()
    );
    for (key, value) in &maps[0] {
        if [
            "host",
            "host_short",
            "uid",
            "user",
            "pane_current_command",
            "pane_current_path",
            "pane_start_command",
            "pane_start_command_list",
            "pane_start_path",
            "session_path",
            "pane_output_generation",
        ]
        .contains(&key.as_str())
        {
            // Host and command spellings differ across the genuine Unix oracle
            // and native Windows. Enumeration must still expose each exact
            // callback value; native identity/path suites verify their source.
            for (index, side) in SIDES.into_iter().enumerate() {
                let output = fixture.run(
                    side,
                    &["display-message", "-p", &format!("#{{{key}}}")],
                    None,
                );
                assert!(output.status.success());
                assert_eq!(
                    output.stdout,
                    format!("{}\n", maps[index][key]).as_bytes(),
                    "{side:?}: {key}"
                );
                assert!(!maps[index][key].is_empty(), "{side:?}: {key}");
            }
            continue;
        }
        if ![
            "history_bytes",
            "history_all_bytes",
            "config_files",
            "version",
            "pid",
            "socket_path",
            "pane_pid",
            "pane_tty",
            "start_time",
            "session_created",
            "session_activity",
            "window_activity",
            "pane_last_output_time",
        ]
        .contains(&key.as_str())
        {
            assert_eq!(maps[1].get(key), Some(value), "{key}");
        }
    }
    let c = fixture.run(
        Side::Reference,
        &["display-message", "-pv", "#{session_name}"],
        None,
    );
    let rust = fixture.run(
        Side::Native,
        &["display-message", "-pv", "#{session_name}"],
        None,
    );
    assert!(c.status.success() && rust.status.success());
    assert!(c.stdout.ends_with(b"test\n"));
    assert_eq!(
        rust.stdout,
        b"tmuxx format input: \"#{session_name}\"\ntmuxx format result: test\ntest\n"
    );
    let long = "x".repeat(5000);
    let rust = fixture.run(Side::Native, &["display-message", "-plv", &long], None);
    assert!(rust.status.success());
    assert_eq!(
        rust.stdout
            .windows(b"[truncated]".len())
            .filter(|window| *window == b"[truncated]")
            .count(),
        2
    );
    assert!(rust.stdout.ends_with(format!("{long}\n").as_bytes()));
}
