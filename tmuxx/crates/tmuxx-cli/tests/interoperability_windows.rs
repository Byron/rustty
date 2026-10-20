#![cfg(windows)]
#[path = "interoperability_windows/common.rs"]
mod common;
use common::*;
use std::time::{Duration, Instant};

fn process_shell(fixture: &Fixture, side: Side, mode: &str, extra: &[&str]) -> String {
    let path = fixture.path(side, "interop_process.py");
    let mut arguments = vec![fixture.python(side), "-u", &path, mode];
    arguments.extend_from_slice(extra);
    arguments
        .into_iter()
        .map(|value| {
            if side == Side::Native {
                assert!(!value.contains(['"', '%', '\r', '\n']));
                format!("\"{value}\"")
            } else {
                format!("'{}'", value.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn stream_large(child: &mut Pending) {
    use std::io::Write;
    if let Pending::Native(owned) = child {
        let mut input = owned.0.as_mut().unwrap().stdin.take().unwrap();
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let writer = std::thread::spawn(move || {
            let result = (|| {
                for _ in 0..257 {
                    input.write_all(&[0; 65536])?;
                }
                input.write_all(b"OVER_16_MIB\n")
            })();
            let _ = sent.send((input, result));
        });
        let result = received.recv_timeout(Duration::from_secs(30));
        if result.is_err() {
            let process = owned.0.as_mut().unwrap();
            let _ = process.kill();
            let _ = process.wait();
        }
        writer.join().unwrap();
        let (input, written) =
            result.expect("native stdin stream stalled beyond the finite input limit");
        written.unwrap();
        owned.0.as_mut().unwrap().stdin = Some(input);
    } else {
        let deadline = Instant::now() + Duration::from_secs(30);
        for _ in 0..257 {
            assert!(
                Instant::now() < deadline,
                "C stdin stream stalled beyond the finite input limit"
            );
            child.write(&[0; 65536]);
        }
        child.write(b"OVER_16_MIB\n");
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn window_creation_policy_aggressive_resize_and_limits_match_c() {
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for side in SIDES {
        let mut first = fixture.control(side);
        first.ok("refresh-client -C 100,40");
        let mut second = fixture.control(side);
        second.ok("refresh-client -C 60,20");
        let mut output = Vec::new();
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -C 120,50");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("select-window -t @0");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -C 110,45 ; display-message -p -t @0 '#{window_width}x#{window_height}'");
        output.push(first.result());
        first.ok("refresh-client -C 120,50");
        second.ok("refresh-client -C @0:45x12");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C @0:");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -f ignore-size");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -C 120,50");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -f ignore-size");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C 60,20");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("refresh-client -f '!ignore-size'");
        second.ok("refresh-client -f '!ignore-size'");
        first.ok("set-option -gw window-size smallest");
        first.ok("refresh-client -C 120,50");
        output.push(first.command(&format!(
            "new-window -d -P -F '#{{window_width}}x#{{window_height}}' {}",
            fixture.child_command(side, "idle", &[])
        )));
        first.ok(&format!(
            "new-session -d -s other {}",
            fixture.child_command(side, "idle", &[])
        ));
        first.ok("link-window -d -s test:0 -t other:1");
        second.ok("switch-client -t other");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        output.push(first.command("set-option -w -t @0 aggressive-resize on"));
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("select-window -t test:1");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("select-window -t other:1");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C @0:45x12");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        output.push(first.command("resize-window -t @0 -x 200 -y 70"));
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        second.ok("refresh-client -C @0:");
        output.push(first.command("display-message -p -t @0 '#{window_width}x#{window_height}'"));
        first.ok("set-option -w -t @1 window-size smallest");
        {
            let mut third = fixture.control(side);
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
            third.ok("refresh-client -C @0:");
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
            third.ok("refresh-client -C @0:30x10");
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
            third.ok("refresh-client -C @0:");
            output
                .push(first.command("display-message -p -t @1 '#{window_width}x#{window_height}'"));
        }
        first.ok("set-option -gw window-size manual");
        first.ok("set-option -t test default-size 20000x1");
        output.push(
            first.command("new-window -d -t test: -P -F '#{window_width}x#{window_height}' ''"),
        );
        output.push(first.command(&format!(
            "new-session -d -s large -x 12000 -y 1 -P -F '#{{window_width}}x#{{window_height}}' {}",
            fixture.child_command(side, "idle", &[]),
        )));
        output.push(first.command("show-options -v -t large default-size"));
        observations.push(output);
    }
    let readable = observations
        .into_iter()
        .map(|output| {
            output
                .into_iter()
                .map(|(lines, failed)| {
                    (
                        lines
                            .into_iter()
                            .map(|line| String::from_utf8_lossy(&line).into_owned())
                            .collect::<Vec<_>>(),
                        failed,
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        readable[1], readable[0],
        "window size policy and creation dimensions"
    );
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn control_subscriptions_flags_sizes_and_flow_match_c() {
    use tmuxx_protocol::Notification;
    let fixture = Fixture::start();
    let legacy_read_only = fixture
        .run(
            Side::Reference,
            &["display-message", "-p", "#{version}"],
            None,
        )
        .stdout
        .starts_with(b"3.7");
    for side in SIDES {
        let admin = |args: &[&str]| {
            let result = fixture.run(side, args, None);
            assert!(result.status.success(), "{side:?}: {args:?}: {result:?}");
            result.stdout
        };
        admin(&["set-option", "-t", "test", "@control-sub", "first"]);
        let mut control = fixture.control(side);
        control.ok("refresh-client -B 'sub::#{@control-sub}' -B 'pane:%*:#{pane_id}'");
        let mut subscriptions = Vec::new();
        for _ in 0..2 {
            subscriptions.push(
                control.notification(|n| matches!(n, Notification::SubscriptionChanged { .. })),
            );
        }
        assert_eq!(
            subscriptions,
            vec![
                Notification::SubscriptionChanged {
                    fields: ["sub", "$0", "-", "-", "-"]
                        .map(|s| s.as_bytes().to_vec())
                        .to_vec(),
                    value: b"first".to_vec()
                },
                Notification::SubscriptionChanged {
                    fields: ["pane", "$0", "@0", "0", "%0"]
                        .map(|s| s.as_bytes().to_vec())
                        .to_vec(),
                    value: b"%0".to_vec()
                },
            ],
            "{side:?}"
        );
        admin(&["set-option", "-t", "test", "@control-sub", "second"]);
        assert_eq!(
            control.notification(|n| matches!(n, Notification::SubscriptionChanged { .. })),
            Notification::SubscriptionChanged {
                fields: ["sub", "$0", "-", "-", "-"]
                    .map(|s| s.as_bytes().to_vec())
                    .to_vec(),
                value: b"second".to_vec()
            }
        );
        control.ok("refresh-client -B sub -B pane");
        control.ok(
            "refresh-client -F no-output,ignore-size -f '!no-output,!ignore-size,pause-after=2'",
        );
        assert_eq!(
            control.ok("display-message -p '#{client_flags}'"),
            [b"attached,focused,control-mode,pause-after=2,UTF-8".to_vec()]
        );
        control.ok("refresh-client -C 90x35");
        assert_eq!(
            control.ok("display-message -p '#{window_width}x#{window_height}'"),
            [b"90x35".to_vec()]
        );
        control.ok("refresh-client -C @0:60x20");
        assert_eq!(
            control.ok("display-message -p '#{window_width}x#{window_height}'"),
            [b"60x20".to_vec()]
        );
        control.ok("refresh-client -C @0:");
        assert_eq!(
            control.ok("display-message -p '#{window_width}x#{window_height}'"),
            [b"90x35".to_vec()]
        );
        assert_eq!(
            control.command("refresh-client -C 0,1"),
            (vec![b"size too small or too big".to_vec()], true)
        );
        admin(&[
            "respawn-pane",
            "-k",
            "-t",
            "%0",
            fixture.python(side),
            "-u",
            &fixture.path(side, "interop_process.py"),
            "echo",
        ]);
        let mut received = Vec::new();
        while !received.ends_with(b"READY") {
            if let Notification::Output { data, age_ms, .. } =
                control.notification(|n| matches!(n, Notification::Output { .. }))
            {
                assert!(age_ms.is_some());
                received.extend(data);
            }
        }
        let paused = control.ok("refresh-client -A '%0:pause'");
        if paused.is_empty() {
            assert_eq!(
                control.notification(|n| matches!(n, Notification::Pause { .. })),
                Notification::Pause { pane_id: 0 }
            );
        } else {
            // tmux 3.7 prints its own flow notification inside the command guard.
            assert_eq!(paused, [b"%pause %0".to_vec()]);
        }
        admin(&["send-keys", "-t", "%0", "-l", "P"]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !admin(&["capture-pane", "-p", "-t", "%0"])
            .windows(6)
            .any(|part| part == b"READYP")
        {
            assert!(
                Instant::now() < deadline,
                "paused pane stopped processing input"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let continued = control.ok("refresh-client -A '%0:continue'");
        if continued.is_empty() {
            assert_eq!(
                control.notification(|n| matches!(n, Notification::Continue { .. })),
                Notification::Continue { pane_id: 0 }
            );
        } else {
            assert_eq!(continued, [b"%continue %0".to_vec()]);
        }
        admin(&["send-keys", "-t", "%0", "-l", "Q"]);
        match control.notification(|n| matches!(n, Notification::Output { .. })) {
            Notification::Output { data, age_ms, .. } => {
                assert_eq!(data, b"Q");
                assert!(age_ms.is_some());
            }
            _ => unreachable!(),
        }
        control.ok("refresh-client -f read-only");
        let denied = if side == Side::Reference && legacy_read_only {
            (Vec::new(), false)
        } else {
            (vec![b"client is read-only".to_vec()], true)
        };
        assert_eq!(control.command("set-option @forbidden yes"), denied);
        control.ok("detach-client");
        drop(control);
        let mut control = fixture.control(side);
        control.ok("refresh-client -f wait-exit");
        control.ok("detach-client");
        control.notification(|n| matches!(n, Notification::Exit { .. }));
        assert!(
            control.exited().is_none(),
            "wait-exit client exited before acknowledgement"
        );
        control.write(b"\n");
        let deadline = Instant::now() + Duration::from_secs(5);
        while control.exited().is_none() {
            assert!(
                Instant::now() < deadline,
                "wait-exit did not accept empty acknowledgement"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn retained_key_panes_and_failed_key_policy_match_c() {
    let fixture = Fixture::start();
    for side in SIDES {
        for name in ["split-window", "new-pane"] {
            let output = fixture.run(
                side,
                &[
                    name,
                    "-dkP",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    "%0",
                    fixture.python(side),
                    "-u",
                    &fixture.path(side, "interop_process.py"),
                    "retained",
                ],
                None,
            );
            assert!(output.status.success(), "{side:?}: {output:?}");
            let pane = std::str::from_utf8(&output.stdout).unwrap().trim();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let state = fixture.run(
                    side,
                    &[
                        "display-message",
                        "-p",
                        "-t",
                        pane,
                        "#{pane_dead}|#{pane_dead_status}|#{cursor_flag}",
                    ],
                    None,
                );
                if state.stdout == b"1|7|0\n" {
                    break;
                }
                assert!(Instant::now() < deadline, "{side:?}: {state:?}");
                std::thread::sleep(Duration::from_millis(5));
            }
            let retained = fixture.run(
                side,
                &["show-options", "-pv", "-t", pane, "remain-on-exit"],
                None,
            );
            assert_eq!(retained.stdout, b"key\n");
            assert!(
                fixture
                    .run(side, &["send-keys", "-t", pane, "x"], None)
                    .status
                    .success()
            );
            assert_eq!(
                fixture
                    .run(
                        side,
                        &["display-message", "-p", "-t", pane, "#{pane_dead}"],
                        None
                    )
                    .stdout,
                b"1\n"
            );
            assert!(
                fixture
                    .run(side, &["kill-pane", "-t", pane], None)
                    .status
                    .success()
            );
        }
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn control_color_reports_query_precedence_and_lifetime_match_c() {
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for (index, side) in SIDES.into_iter().enumerate() {
        let leaf = format!("colors-{index}");
        let run = |args: &[&str]| {
            let output = fixture.run(side, args, None);
            assert!(output.status.success(), "{args:?}: {output:?}");
            output.stdout
        };
        let mut round = 0;
        let mut query = || {
            run(&["send-keys", "-t", "%0", "-l", "q"]);
            round += 1;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let lines = String::from_utf8(fixture.maybe_get(side, &leaf)).unwrap();
                if lines.lines().count() == round {
                    return lines.lines().last().unwrap().to_owned();
                }
                assert!(Instant::now() < deadline, "query response missing: {lines}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let spawn = || {
            run(&[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                fixture.python(side),
                "-u",
                &fixture.path(side, "interop_process.py"),
                "colors",
                &fixture.path(side, &leaf),
            ]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !run(&["capture-pane", "-p", "-t", "%0"]).starts_with(b"READY") {
                assert!(Instant::now() < deadline, "query process not ready");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        spawn();
        let mut output = vec![query()];
        let mut first = fixture.control(side);
        let second = fixture.control(side);
        let report = |client: &mut ControlStream, channel: u8, value: &str| {
            let argument = tmuxx_command::display_argument(
                format!("%0:\x1b]{channel};{value}\x07").as_bytes(),
            );
            client.ok(&format!(
                "refresh-client -r {}",
                String::from_utf8(argument).unwrap()
            ));
        };
        for (channel, value) in [
            (10, "rgb:8000/ab00/ffff"),
            (11, "#ffffff"),
            (10, "#123"),
            (11, "cmyk:0/0/0/0.5"),
            (10, "1,-2,259junk"),
            (11, " gray50 "),
        ] {
            report(&mut first, channel, value);
            output.push(query());
        }
        run(&[
            "set-option",
            "-g",
            "window-active-style",
            "fg=#ffff00,bg=#ffffff",
        ]);
        output.push(query());
        drop(first);
        output.push(query());
        drop(second);
        output.push(query());
        let third = fixture.control(side);
        output.push(query());
        spawn();
        output.push(query());
        drop(third);
        observations.push(output);
    }
    assert_eq!(
        observations[1], observations[0],
        "query-only control colors, disconnect and respawn"
    );
}

#[test]
#[ignore = "requires HEAD C pane_last_output_time and real Unix sockets/PTYs"]
fn process_exit_signal_pipe_and_output_format_lifetimes_match_c() {
    process_lifetimes(true);
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn process_exit_signal_and_pipe_lifetimes_match_supported_c() {
    process_lifetimes(false);
}

fn process_lifetimes(output_time: bool) {
    let fixture = Fixture::start();
    let mut observations = Vec::new();
    for side in SIDES {
        let run = |args: &[&str]| {
            let result = fixture.run(side, args, None);
            assert!(result.status.success(), "{args:?}: {result:?}");
            String::from_utf8(result.stdout).unwrap()
        };
        let fields = || {
            run(&[
                "display-message",
                "-p",
                "-t",
                "%0",
                if output_time {
                    "#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{?pane_dead_time,1,0}|#{pane_pipe}|#{?pane_pipe_pid,1,0}|#{?pane_last_output_time,1,0}"
                } else {
                    "#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{?pane_dead_time,1,0}|#{pane_pipe}|#{?pane_pipe_pid,1,0}"
                },
            ])
        };
        let wait = |wanted: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let output = run(&["capture-pane", "-p", "-t", "%0"]);
                if output.contains(wanted) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "process marker missing: {output}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        run(&["set-option", "-w", "remain-on-exit", "on"]);
        let mut output = Vec::new();
        for ending in ["status", "terminated"] {
            run(&[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                fixture.python(side),
                "-u",
                &fixture.path(side, "interop_process.py"),
                "exit",
                ending,
            ]);
            wait("READY");
            output.push(fields());
            run(&[
                "pipe-pane",
                "-t",
                "%0",
                &process_shell(&fixture, side, "sink", &[]),
            ]);
            output.push(fields());
            let pane_pid = run(&["display-message", "-p", "-t", "%0", "#{pane_pid}"]);
            let pipe_pid = run(&["display-message", "-p", "-t", "%0", "#{pane_pipe_pid}"]);
            assert!(pipe_pid.trim().parse::<u32>().unwrap() > 0 && pipe_pid != pane_pid);
            if !output_time {
                run(&["pipe-pane", "-t", "%0"]);
                output.push(fields());
            }
            run(&["send-keys", "-t", "%0", "-l", "q"]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fields().starts_with("1|") {
                assert!(Instant::now() < deadline, "process did not exit");
                std::thread::sleep(Duration::from_millis(5));
            }
            let exited = fields();
            // Win32 termination is an exit status, never a POSIX wait signal.
            // Keep the platform-native result explicit before comparing the
            // shared lifetime fields; do not infer SIGKILL from status 137.
            if ending == "terminated" && side == Side::Native {
                assert!(exited.starts_with("1|137||"), "{exited}");
                output.push(exited.replacen("1|137||", "1||9|", 1));
            } else {
                output.push(exited);
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            for key in ["pane_dead_time", "pane_last_output_time"] {
                if key == "pane_last_output_time" && !output_time {
                    continue;
                }
                let time = run(&["display-message", "-p", "-t", "%0", &format!("#{{{key}}}")])
                    .trim()
                    .parse::<u64>()
                    .unwrap();
                assert!(time <= now && now - time < 10, "{key}: {time} versus {now}");
            }
        }
        observations.push(output);
    }
    assert_eq!(
        observations[1], observations[0],
        "process signal, timestamp, respawn and pipe lifetimes"
    );
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn empty_respawn_and_pipe_lifecycle_match_c() {
    let fixture = Fixture::start();
    let legacy_pid = fixture
        .run(
            Side::Reference,
            &["display-message", "-p", "#{version}"],
            None,
        )
        .stdout
        .starts_with(b"3.7");
    for side in SIDES {
        let run = |args: &[&str], input| {
            let result = fixture.run(side, args, input);
            assert!(result.status.success(), "{side:?}: {args:?}: {result:?}");
            result.stdout
        };
        let capture = |pane: &str, expected: &[u8]| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let output = run(&["capture-pane", "-p", "-t", pane], None);
                if output.windows(expected.len()).any(|part| part == expected) {
                    return output;
                }
                assert!(
                    Instant::now() < deadline,
                    "{side:?}: missing {expected:?}: {output:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let empty = run(
            &[
                "split-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "%0",
                "",
            ],
            None,
        );
        let empty = std::str::from_utf8(&empty).unwrap().trim();
        assert_eq!(
            run(
                &[
                    "display-message",
                    "-p",
                    "-t",
                    empty,
                    "#{pane_pid}|#{pane_dead}|#{cursor_flag}"
                ],
                None
            ),
            if side == Side::Reference && legacy_pid {
                b"0|0|0\n".as_slice()
            } else {
                b"|0|0\n".as_slice()
            }
        );
        run(
            &["display-message", "-I", "-t", empty],
            Some(b"one\ntwo\n\x1b[31mred"),
        );
        let captured = run(
            &[
                "capture-pane",
                "-p",
                "-e",
                "-t",
                empty,
                "-S",
                "0",
                "-E",
                "2",
            ],
            None,
        );
        assert_eq!(captured, b"one\ntwo\n\x1b[31mred\x1b[39m\n");
        run(&["send-keys", "-t", empty, "-l", "ignored"], None);
        assert_eq!(
            run(
                &[
                    "capture-pane",
                    "-p",
                    "-e",
                    "-t",
                    empty,
                    "-S",
                    "0",
                    "-E",
                    "2"
                ],
                None
            ),
            captured
        );
        run(
            &[
                "respawn-pane",
                "-t",
                empty,
                fixture.python(side),
                "-u",
                &fixture.path(side, "interop_process.py"),
                "mark",
                "REVIVED",
            ],
            None,
        );
        capture(empty, b"REVIVED");
        let failed = fixture.run(side, &["respawn-window", "-t", "test:0"], None);
        assert!(!failed.status.success());
        assert_eq!(
            failed.stderr,
            b"respawn window failed: window test:0 still active\n"
        );
        run(
            &[
                "respawn-window",
                "-k",
                "-t",
                "test:0",
                "-e",
                "TMUXX_PROCESS_MARK=REPLACED",
                fixture.python(side),
                "-u",
                &fixture.path(side, "interop_process.py"),
                "env-mark",
                "TMUXX_PROCESS_MARK",
            ],
            None,
        );
        capture("%0", b"REPLACED");
        assert_eq!(
            run(
                &[
                    "list-panes",
                    "-t",
                    "test:0",
                    "-F",
                    "#{pane_id}|#{pane_active}|#{pane_width}x#{pane_height}"
                ],
                None
            ),
            b"%0|1|80x24\n"
        );
        let streamed = run(
            &[
                "split-window",
                "-d",
                "-I",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "%0",
            ],
            Some(b"stdin\nsecond"),
        );
        let streamed = std::str::from_utf8(&streamed).unwrap().trim();
        capture(streamed, b"stdin\nsecond");
        run(&["kill-pane", "-t", streamed], None);

        run(
            &[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                fixture.python(side),
                "-u",
                &fixture.path(side, "interop_process.py"),
                "echo",
            ],
            None,
        );
        capture("%0", b"READY");
        let log = fixture.path(side, "pipe");
        let pipe = process_shell(&fixture, side, "sink", &[&log]);
        run(&["pipe-pane", "-t", "%0", &pipe], None);
        assert_eq!(
            run(&["display-message", "-p", "-t", "%0", "#{pane_pipe}"], None),
            b"1\n"
        );
        run(&["send-keys", "-t", "%0", "-l", "A"], None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture.maybe_get(side, "pipe") != b"A" {
            assert!(
                Instant::now() < deadline,
                "pipe did not capture raw output once"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let observer = fixture.control(side);
        run(
            &[
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                fixture.python(side),
                "-u",
                &fixture.path(side, "interop_process.py"),
                "echo",
            ],
            None,
        );
        capture("%0", b"READY");
        // Pipe output is the actual PTY byte stream. ConPTY starts each new
        // pseudoconsole with real VT negotiations; compare those bytes to an
        // independent control observer rather than stripping escape sequences.
        let mut expected_pipe = b"A".to_vec();
        loop {
            if let tmuxx_protocol::Notification::Output { data, .. } =
                observer.notification(|value| {
                    matches!(
                        value,
                        tmuxx_protocol::Notification::Output { pane_id: 0, .. }
                    )
                })
            {
                expected_pipe.extend(data);
            }
            if expected_pipe.ends_with(b"READY") {
                break;
            }
        }
        if side == Side::Reference {
            assert_eq!(expected_pipe, b"AREADY");
        }
        assert_eq!(
            expected_pipe
                .windows(5)
                .filter(|part| *part == b"READY")
                .count(),
            1
        );
        drop(observer);
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture.maybe_get(side, "pipe") != expected_pipe {
            assert!(
                Instant::now() < deadline,
                "pipe did not survive respawn without duplicated output: {:?}",
                fixture.maybe_get(side, "pipe")
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        run(&["pipe-pane", "-o", "-t", "%0", &pipe], None);
        assert_eq!(
            run(&["display-message", "-p", "-t", "%0", "#{pane_pipe}"], None),
            b"0\n"
        );
        run(&["send-keys", "-t", "%0", "-l", "B"], None);
        capture("%0", b"READYB");
        assert_eq!(fixture.get(side, "pipe"), expected_pipe);
        run(
            &[
                "pipe-pane",
                "-I",
                "-t",
                "%0",
                &process_shell(&fixture, side, "write", &["Q"]),
            ],
            None,
        );
        capture("%0", b"READYBQ");
        let deadline = Instant::now() + Duration::from_secs(5);
        while run(&["display-message", "-p", "-t", "%0", "#{pane_pipe}"], None) != b"0\n" {
            assert!(Instant::now() < deadline, "pipe child was not reaped");
            std::thread::sleep(Duration::from_millis(5));
        }
        run(
            &[
                "pipe-pane",
                "-IO",
                "-t",
                "%0",
                &process_shell(&fixture, side, "duplex", &[]),
            ],
            None,
        );
        run(&["send-keys", "-t", "%0", "-l", "S"], None);
        capture("%0", b"READYBQSR");

        let producer = run(
            &[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                fixture.python(side),
                "-u",
                &fixture.path(side, "interop_process.py"),
                "burst",
            ],
            None,
        );
        let producer = std::str::from_utf8(&producer).unwrap().trim();
        capture(producer, b"READY");
        let drained = fixture.path(side, "drained");
        let consumer = process_shell(&fixture, side, "slow-sink", &[&drained]);
        run(&["pipe-pane", "-t", producer, &consumer], None);
        run(&["select-window", "-t", producer], None);
        let observer = fixture.control(side);
        let producer_id = producer.strip_prefix('%').unwrap().parse::<u64>().unwrap();
        let window = run(&["display", "-p", "-t", producer, "#{window_id}"], None);
        let window_id = std::str::from_utf8(&window)
            .unwrap()
            .trim()
            .strip_prefix('@')
            .unwrap()
            .parse::<u64>()
            .unwrap();
        run(&["send-keys", "-t", producer, "-l", "X"], None);
        let mut expected_raw = Vec::new();
        loop {
            match observer.next() {
                tmuxx_protocol::Event::Notification(tmuxx_protocol::Notification::Output {
                    pane_id,
                    data,
                    ..
                }) if pane_id == producer_id => expected_raw.extend(data),
                tmuxx_protocol::Event::Notification(
                    tmuxx_protocol::Notification::WindowClose {
                        window_id: closed, ..
                    },
                ) if closed == window_id => break,
                _ => {}
            }
            assert!(
                expected_raw.len() <= 2 * 1024 * 1024,
                "unbounded burst output"
            );
        }
        assert_eq!(
            expected_raw.iter().filter(|byte| **byte == b'Z').count(),
            262144,
            "{side:?} burst did not preserve all glyph output"
        );
        if side == Side::Reference {
            assert_eq!(expected_raw, vec![b'Z'; 262144]);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let bytes = fixture.maybe_get(side, "drained");
            if bytes == expected_raw {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pipe lost trailing output at process exit: {} bytes",
                bytes.len()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn live_stdin_streams_before_eof_and_cancels_with_c() {
    let fixture = Fixture::start();
    let new_pane = fixture
        .run(
            Side::Reference,
            &["list-commands", "-F", "#{command_list_name}"],
            None,
        )
        .stdout
        .windows(9)
        .any(|name| name == b"new-pane\n");
    for side in SIDES {
        let run = |args: &[&str]| {
            let output = fixture.run(side, args, None);
            assert!(output.status.success(), "{side:?} {args:?}: {output:?}");
            output.stdout
        };
        let base = run(&[
            "new-window",
            "-dP",
            "-F",
            "#{pane_id}",
            fixture.python(side),
            "-u",
            &fixture.path(side, "interop_process.py"),
            "idle",
        ]);
        let base = std::str::from_utf8(&base).unwrap().trim();
        for name in if new_pane {
            vec!["split-window", "new-pane"]
        } else {
            vec!["split-window"]
        } {
            run(&["set-option", "-g", "@stdin-hook", "pending"]);
            run(&[
                "set-hook",
                "-g",
                "after-split-window",
                "set-option -g @stdin-hook done",
            ]);
            let mut child = fixture.spawn(
                side,
                &[
                    name,
                    "-dIP",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    base,
                    ";",
                    "display-message",
                    "-p",
                    "AFTER",
                ],
            );
            child.write(b"BEFORE_EOF\n");
            let deadline = Instant::now() + Duration::from_secs(5);
            let pane = loop {
                let panes = run(&["list-panes", "-t", base, "-F", "#{pane_id}"]);
                if let Some(pane) = std::str::from_utf8(&panes)
                    .unwrap()
                    .lines()
                    .find(|pane| *pane != base)
                {
                    break pane.to_owned();
                }
                assert!(
                    Instant::now() < deadline,
                    "{side:?} {name}: stdin pane was not created before EOF: {panes:?}; child exit {:?}",
                    child.exited()
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            loop {
                if run(&["capture-pane", "-p", "-t", &pane]).starts_with(b"BEFORE_EOF") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "stdin was not rendered before EOF"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(run(&["show-options", "-gv", "@stdin-hook"]), b"pending\n");
            assert!(child.exited().is_none(), "stdin command exited before EOF");
            // Cross the finite Request limit without accumulating bytes in the server.
            if name == "split-window" {
                stream_large(&mut child);
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if run(&["capture-pane", "-p", "-t", &pane])
                        .windows(11)
                        .any(|part| part == b"OVER_16_MIB")
                    {
                        break;
                    }
                    assert!(Instant::now() < deadline, "large stream was truncated");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            child.finish_input();
            let output = child.output();
            assert!(output.status.success(), "{side:?}: {output:?}");
            assert_eq!(output.stdout, format!("{pane}\nAFTER\n").as_bytes());
            assert_eq!(run(&["show-options", "-gv", "@stdin-hook"]), b"done\n");
            run(&["set-hook", "-gu", "after-split-window"]);
            run(&["kill-pane", "-t", &pane]);
        }
        let display = run(&["split-window", "-dP", "-F", "#{pane_id}", "-t", base, ""]);
        let display = std::str::from_utf8(&display).unwrap().trim();
        let mut child = fixture.spawn(
            side,
            &[
                "display-message",
                "-I",
                "-t",
                display,
                ";",
                "display-message",
                "-p",
                "AFTER_CANCEL",
            ],
        );
        child.write(b"DISPLAY_LIVE\n");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !run(&["capture-pane", "-p", "-t", display]).starts_with(b"DISPLAY_LIVE") {
            assert!(Instant::now() < deadline, "display -I waited for EOF");
            std::thread::sleep(Duration::from_millis(5));
        }
        run(&["kill-pane", "-t", display]);
        // C notices removed input panes on the next input callback.
        match &mut child {
            Pending::Reference(control) => {
                let _ = control.write(b"cancel");
            }
            Pending::Native(owned) => {
                use std::io::Write;
                let _ = owned
                    .0
                    .as_mut()
                    .unwrap()
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(b"cancel");
            }
        }
        let output = child.output();
        assert!(
            !output.status.success(),
            "removed pane left a successful input request: {output:?}"
        );
        assert!(
            !output
                .stdout
                .windows(12)
                .any(|part| part == b"AFTER_CANCEL")
        );
        run(&["kill-window", "-t", base]);
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and native packaged ConPTY"]
fn input_shutdown_and_pane_wait_follow_c() {
    let fixture = Fixture::start();
    let reference_has_wait = fixture
        .run(
            Side::Reference,
            &[
                "list-commands",
                "-F",
                "#{command_list_usage}",
                "split-window",
            ],
            None,
        )
        .stdout
        .contains(&b'W');
    for side in SIDES {
        if reference_has_wait {
            let waited = fixture.run(
                side,
                &[
                    "split-window",
                    "-dWP",
                    "-F",
                    "WAITED",
                    "-t",
                    "%0",
                    fixture.python(side),
                    "-u",
                    &fixture.path(side, "interop_process.py"),
                    "exit-now",
                    "7",
                    ";",
                    "display-message",
                    "-p",
                    "AFTER_WAIT",
                ],
                None,
            );
            assert_eq!(waited.status.code(), Some(7), "{side:?}: {waited:?}");
            assert_eq!(waited.stdout, b"WAITED\nAFTER_WAIT\n");
            let signalled = fixture.run(
                side,
                &[
                    "split-window",
                    "-dW",
                    "-t",
                    "%0",
                    fixture.python(side),
                    "-u",
                    &fixture.path(side, "interop_process.py"),
                    "terminate-now",
                    "143",
                ],
                None,
            );
            assert_eq!(
                signalled.status.code(),
                Some(143),
                "{side:?}: {signalled:?}"
            );
            let mut child = fixture.spawn(
                side,
                &[
                    "split-window",
                    "-dW",
                    "-t",
                    "%0",
                    "",
                    ";",
                    "display-message",
                    "-p",
                    "AFTER_REMOVAL",
                ],
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            let pane = loop {
                let listed =
                    fixture.run(side, &["list-panes", "-t", "%0", "-F", "#{pane_id}"], None);
                if let Some(pane) = std::str::from_utf8(&listed.stdout)
                    .unwrap()
                    .lines()
                    .find(|pane| *pane != "%0")
                {
                    break pane.to_owned();
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(child.exited().is_none());
            assert!(
                fixture
                    .run(side, &["kill-pane", "-t", &pane], None)
                    .status
                    .success()
            );
            let removed = child.output();
            assert_eq!(removed.status.code(), Some(129), "{side:?}: {removed:?}");
            assert_eq!(removed.stdout, b"AFTER_REMOVAL\n");
        }
        let empty = fixture.run(
            side,
            &["split-window", "-dP", "-F", "#{pane_id}", "-t", "%0", ""],
            None,
        );
        assert!(empty.status.success());
        let pane = std::str::from_utf8(&empty.stdout).unwrap().trim();
        let mut child = fixture.spawn(side, &["display-message", "-I", "-t", pane]);
        child.write(b"SHUTDOWN_READY\n");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fixture
                .run(side, &["capture-pane", "-p", "-t", pane], None)
                .stdout
                .starts_with(b"SHUTDOWN_READY")
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = fixture.run(side, &["kill-server"], None);
        let stopped = child.output();
        assert!(stopped.status.success(), "{side:?}: {stopped:?}");
    }
}

#[test]
#[ignore = "requires native packaged ConPTY"]
fn native_cli_streams_input_before_eof() {
    use std::io::Write;
    let endpoint =
        tmuxx_runtime::windows::endpoint(&format!("tmuxx-native-stdin-{}", std::process::id()))
            .unwrap();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&endpoint)).unwrap();
    let client = std::path::Path::new(env!("CARGO_BIN_EXE_tmuxx"));
    let run = |args: &[&str]| {
        let output = std::process::Command::new(client)
            .arg("--native-endpoint")
            .arg(&endpoint)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        output.stdout
    };
    let python = std::env::var("TMUXX_REMOTE_PYTHON").unwrap();
    run(&[
        "new-session",
        "-d",
        &python,
        "-c",
        "import time; time.sleep(120)",
    ]);
    let pane = run(&["split-window", "-dP", "-F", "#{pane_id}", ""]);
    let pane = std::str::from_utf8(&pane).unwrap().trim();
    let mut child = OwnedChild(Some(
        std::process::Command::new(client)
            .arg("--native-endpoint")
            .arg(&endpoint)
            .args([
                "display-message",
                "-I",
                "-t",
                pane,
                ";",
                "display-message",
                "-p",
                "AFTER",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let mut input = child.0.as_mut().unwrap().stdin.take().unwrap();
    input.write_all(b"NATIVE_BEFORE_EOF\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let captured = run(&["capture-pane", "-p", "-t", pane]);
        if captured.starts_with(b"NATIVE_BEFORE_EOF") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native live input capture: {captured:?}; child exit {:?}",
            child.0.as_mut().unwrap().try_wait()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none());
    drop(input);
    let output = wait_native(&mut child);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"AFTER\n");
    host.shutdown().unwrap();
}
