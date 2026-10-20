#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Request, ServerMessage};
use tmuxx_runtime::{LocalClient, Options, ServerHandle};

trait Commands {
    fn run(&self, args: &[&str]) -> (i32, Vec<u8>);
}
struct Rust {
    host: ServerHandle,
    client: LocalClient,
}
fn path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[cfg(windows)]
    return tmuxx_runtime::windows::endpoint(&format!(
        "hooks-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap();
    #[cfg(unix)]
    std::env::temp_dir().join(format!(
        "tx-hook-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
impl Rust {
    fn new() -> Self {
        let host = tmuxx_runtime::start(Options::new(path())).unwrap();
        let client = host.connect().unwrap();
        Self { host, client }
    }
}
impl Commands for Rust {
    fn run(&self, args: &[&str]) -> (i32, Vec<u8>) {
        run_client(&self.client, args)
    }
}
fn run_client(client: &LocalClient, args: &[&str]) -> (i32, Vec<u8>) {
    #[cfg(unix)]
    let argv = args.iter().map(|v| v.as_bytes().to_vec()).collect();
    #[cfg(windows)]
    let argv = fixture_argv(args);
    client
        .send(Request {
            request_id: 1,
            argv,
            stdin: None,
            subscribe: false,
            control: false,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(response)) = client.try_recv() {
            return (
                if response.error.is_some() {
                    1
                } else {
                    response.status.unwrap_or(0)
                },
                response.output,
            );
        }
        assert!(Instant::now() < deadline, "no reply to {args:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[cfg(windows)]
fn fixture_argv(args: &[&str]) -> Vec<Vec<u8>> {
    // These are the original test's passive and immediately exiting pane
    // applications. Translate only these explicit process fixtures, retaining
    // every command, target, hook source and observed output unchanged.
    let mut values = args
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    if matches!(
        args.first().copied(),
        Some("new-session" | "new-window" | "neww" | "split-window")
    ) {
        let fixture = if args.ends_with(&["sleep", "30"]) {
            Some((2, "import time; time.sleep(30)"))
        } else if args.ends_with(&["sleep 30"]) {
            Some((1, "import time; time.sleep(30)"))
        } else if args.ends_with(&["/bin/sh", "-c", "exit 0"]) {
            Some((3, "raise SystemExit(0)"))
        } else {
            None
        };
        if let Some((count, source)) = fixture {
            values.truncate(values.len() - count);
            values.extend([
                std::env::var("TMUXX_REMOTE_PYTHON").unwrap_or_else(|_| "python".into()),
                "-c".into(),
                source.into(),
            ]);
        }
    }
    values.into_iter().map(String::into_bytes).collect()
}
fn good(server: &impl Commands, args: &[&str]) -> Vec<u8> {
    let (status, out) = server.run(args);
    assert_eq!(status, 0, "failed: {args:?}; output {out:?}");
    out
}
fn value(server: &impl Commands, name: &str) -> Vec<u8> {
    good(server, &["show-options", "-gqv", name])
}
fn wait_value(server: &impl Commands, name: &str, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = value(server, name);
        if current == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name}: expected {expected:?}, got {current:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn setup(server: &impl Commands) {
    good(
        server,
        &[
            "new-session",
            "-d",
            "-s",
            "main",
            "-n",
            "first",
            "sleep",
            "30",
        ],
    );
    good(
        server,
        &[
            "new-session",
            "-d",
            "-s",
            "other",
            "-n",
            "second",
            "sleep",
            "30",
        ],
    );
}
fn after_contract(server: &impl Commands) -> Vec<Vec<u8>> {
    setup(server);
    good(
        server,
        &[
            "set-hook",
            "-g",
            "after-rename-window",
            "set -gF @after \"#{hook}|#{hook_arguments}|#{hook_argument_0}|#{hook_flag_t}|#{session_name}\"",
        ],
    );
    good(server, &["rename-window", "-t", "other:0", "renamed"]);
    let mut observations = vec![
        value(server, "@after"),
        good(server, &["show-hooks", "-g", "after-rename-window"]),
    ];
    good(server, &["set-hook", "-gu", "after-rename-window"]);
    good(server, &["set", "-g", "@order", ""]);
    good(
        server,
        &[
            "set-hook",
            "-g",
            "after-rename-window[9]",
            "set -gF @order '#{@order}9'",
        ],
    );
    good(
        server,
        &[
            "set-hook",
            "-g",
            "after-rename-window[2]",
            "set -gF @order '#{@order}2'",
        ],
    );
    good(
        server,
        &[
            "set-hook",
            "-ga",
            "after-rename-window",
            "set -gF @order '#{@order}0'",
        ],
    );
    good(
        server,
        &[
            "rename-window",
            "-t",
            "main:0",
            "indexed",
            ";",
            "set",
            "-gF",
            "@order",
            "#{@order}N",
        ],
    );
    observations.push(value(server, "@order"));
    observations.push(good(server, &["show-hooks", "-g", "after-rename-window"]));
    good(server, &["set-hook", "-gu", "after-rename-window[2]"]);
    observations.push(good(server, &["show-hooks", "-g", "after-rename-window"]));
    good(
        server,
        &[
            "set-hook",
            "-t",
            "other:0",
            "after-rename-window",
            "set -g @local local",
        ],
    );
    good(server, &["set", "-g", "@order", ""]);
    good(server, &["rename-window", "-t", "other:0", "local"]);
    observations.push(value(server, "@order"));
    observations.push(value(server, "@local"));
    good(
        server,
        &["set-hook", "-u", "-t", "other:0", "after-rename-window"],
    );
    good(server, &["set-hook", "-gu", "after-rename-window"]);
    good(server, &["set", "-g", "@recursive", "no"]);
    good(
        server,
        &[
            "set-hook",
            "-g",
            "after-set-environment",
            "set -g @recursive bad",
        ],
    );
    good(
        server,
        &[
            "set-hook",
            "-g",
            "after-rename-window",
            "set-environment -g HOOK_ENV yes",
        ],
    );
    good(server, &["rename-window", "-t", "main:0", "nonrecursive"]);
    observations.push(value(server, "@recursive"));
    good(
        server,
        &["set-hook", "-g", "command-error", "set -g @error ran"],
    );
    let failed = server.run(&[
        "rename-window",
        "-t",
        "missing:0",
        "bad",
        ";",
        "set",
        "-g",
        "@cancelled",
        "bad",
    ]);
    assert_ne!(failed.0, 0);
    observations.push(value(server, "@error"));
    observations.push(value(server, "@cancelled"));
    good(
        server,
        &[
            "set-hook",
            "-g",
            "@manual",
            "set -gF @manual-result '#{hook}:#{session_name}:#{pane_id}'",
        ],
    );
    good(server, &["set-hook", "-gR", "-t", "other:0.0", "@manual"]);
    observations.push(value(server, "@manual-result"));
    good(
        server,
        &[
            "set-hook",
            "-g",
            "after-rename-window",
            "select-pane -t %99999 ; set -g @hook-cancelled bad",
        ],
    );
    good(
        server,
        &[
            "set-hook",
            "-ga",
            "after-rename-window",
            "set -g @other-hook ran",
        ],
    );
    let failed = server.run(&[
        "rename-window",
        "-t",
        "main:0",
        "hook-error",
        ";",
        "set",
        "-g",
        "@after-error",
        "ran",
    ]);
    assert_ne!(failed.0, 0);
    observations.push(value(server, "@hook-cancelled"));
    observations.push(value(server, "@other-hook"));
    observations.push(value(server, "@after-error"));
    observations
}
fn lifecycle_contract(server: &impl Commands) -> Vec<Vec<u8>> {
    setup(server);
    good(server, &["set", "-g", "@log", ""]);
    good(
        server,
        &[
            "set-hook",
            "-g",
            "pane-exited",
            "set -gF @log '#{@log}|pane-exited:#{hook_pane}'",
        ],
    );
    good(
        server,
        &[
            "set-hook",
            "-g",
            "session-closed",
            "set -gF @log '#{@log}|session-closed:#{hook_session_name}'",
        ],
    );
    good(
        server,
        &[
            "set-hook",
            "-g",
            "window-unlinked",
            "set -gF @log '#{@log}|window-unlinked:#{hook_session_name}:#{hook_window_name}'",
        ],
    );
    let pane = good(
        server,
        &[
            "new-session",
            "-d",
            "-s",
            "doomed",
            "-n",
            "dead",
            "-P",
            "-F",
            "#{pane_id}",
            "/bin/sh",
            "-c",
            "exit 0",
        ],
    );
    let expected = format!(
        "|pane-exited:{}|session-closed:doomed|window-unlinked:doomed:dead\n",
        String::from_utf8_lossy(&pane).trim()
    );
    wait_value(server, "@log", expected.as_bytes());
    let mut observations = vec![value(server, "@log")];
    good(server, &["set", "-g", "@log", ""]);
    good(server, &["kill-session", "-t", "other"]);
    wait_value(
        server,
        "@log",
        b"|session-closed:other|window-unlinked:other:second\n",
    );
    observations.push(value(server, "@log"));
    good(
        server,
        &[
            "set-hook",
            "-g",
            "pane-died",
            "set -gF @died '#{hook_pane}' ; kill-pane",
        ],
    );
    good(
        server,
        &["set-window-option", "-t", "main:0", "remain-on-exit", "on"],
    );
    let pane = good(
        server,
        &[
            "split-window",
            "-d",
            "-t",
            "main:0",
            "-P",
            "-F",
            "#{pane_id}",
            "/bin/sh",
            "-c",
            "exit 0",
        ],
    );
    wait_value(server, "@died", &pane);
    observations.push(value(server, "@died"));
    assert_ne!(
        server
            .run(&["select-pane", "-t", String::from_utf8_lossy(&pane).trim()])
            .0,
        0
    );
    observations
}
#[test]
fn after_hooks_have_scoped_targets_isolated_errors_and_no_recursion() {
    let server = Rust::new();
    let out = after_contract(&server);
    assert_eq!(
        out[0],
        b"after-rename-window|-t other:0 renamed|renamed|other:0|other\n"
    );
    assert_eq!(out[2], b"029N\n");
    drop(server.host);
}
#[test]
fn dead_object_hook_payloads_survive_removal_and_target_the_remaining_dead_pane() {
    let server = Rust::new();
    lifecycle_contract(&server);
    drop(server.host);
}

struct C {
    #[cfg(unix)]
    program: PathBuf,
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
}
impl C {
    fn new() -> Self {
        #[cfg(windows)]
        return Self {
            remote: remote_reference::RemoteReference::new(),
        };
        #[cfg(unix)]
        Self {
            program: std::env::var_os("TMUXX_C_TMUX")
                .expect("set TMUXX_C_TMUX")
                .into(),
            socket: path(),
        }
    }
}
impl Commands for C {
    fn run(&self, args: &[&str]) -> (i32, Vec<u8>) {
        #[cfg(windows)]
        let out = self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        );
        #[cfg(unix)]
        let out = {
            use std::process::{Command, Stdio};
            let mut child = Command::new(&self.program)
                .arg("-S")
                .arg(&self.socket)
                .args(["-f", "/dev/null"])
                .args(args)
                .env_remove("TMUX")
                .env("SHELL", "/bin/sh")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("C command timeout {args:?}");
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            child.wait_with_output().unwrap()
        };
        (out.status.code().unwrap_or(1), out.stdout)
    }
}
#[cfg(unix)]
impl Drop for C {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}
#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn after_hook_array_context_order_and_error_contract_matches_c() {
    assert_eq!(after_contract(&Rust::new()), after_contract(&C::new()));
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn window_navigation_hooks_use_selected_targets_and_aliases_match_c() {
    struct OneShot(Rust);
    impl Commands for OneShot {
        fn run(&self, args: &[&str]) -> (i32, Vec<u8>) {
            run_client(&self.0.host.connect().unwrap(), args)
        }
    }
    fn contract(server: &impl Commands) -> Vec<Vec<u8>> {
        setup(server);
        good(server, &["neww", "-d", "-t", "main:3", "sleep 30"]);
        good(server, &["neww", "-d", "-t", "main:7", "sleep 30"]);
        good(server, &["neww", "-d", "-t", "other:3", "sleep 30"]);
        good(server, &["set", "-g", "@selected", ""]);
        good(
            server,
            &[
                "set-hook",
                "-g",
                "after-select-window",
                "set -agF @selected '#{session_name}:#{window_index}:#{pane_id}|'",
            ],
        );
        good(
            server,
            &[
                "set-hook",
                "-t",
                "main",
                "after-select-window",
                "set -agF @selected 'main-hook:#{session_name}:#{window_index}:#{pane_id}|'",
            ],
        );
        let mut observations = Vec::new();
        for args in [
            vec!["selectw", "-n", "-t", "main:7"],
            vec!["next", "-t", "main"],
            vec!["prev", "-t", "main"],
            vec!["last", "-t", "main"],
            vec!["selectw", "-T", "-t", "main:7"],
            vec!["selectw", "-t", "main:3"],
            vec!["selectw", "-T", "-t", "main"],
            vec!["selectw", "-T", "-t", "main:0"],
            vec!["selectw", "-T", "-t", "other:3"],
            vec!["selectw", "-T", "-t", "other:3"],
            vec!["selectw", "-t", "other:0"],
            vec!["next", "-a", "-t", "main"],
        ] {
            observations.push(server.run(&args).0.to_string().into_bytes());
            observations.push(value(server, "@selected"));
        }
        observations
    }
    assert_eq!(contract(&OneShot(Rust::new())), contract(&C::new()));
}

#[test]
#[ignore = "requires current C hook option metadata and real PTYs"]
fn hook_option_metadata_keys_and_firing_counts_match_c() {
    fn contract(server: &impl Commands) -> Vec<Vec<u8>> {
        const FIELDS: &str = "#{option_name}:#{option_array_key}:#{option_is_hook}:#{option_is_user}:#{option_is_array}:#{option_is_string}:#{option_has_value}:#{option_has_array_key}:#{option_value}:#{hook_fire_count}:#{>:#{hook_fire_time},0}:#{hook_monitor_target}:#{hook_monitor_format}";
        setup(server);
        let mut out = vec![good(
            server,
            &["show-hooks", "-gF", FIELDS, "after-rename-window"],
        )];
        good(server, &["set-hook", "-R", "after-rename-window"]);
        out.push(good(
            server,
            &["show-hooks", "-gF", FIELDS, "after-rename-window"],
        ));
        for (name, value) in [
            ("after-rename-window[0002]", "set -g @two 2"),
            ("after-rename-window[+1]", "set -g @plus 1"),
            ("after-rename-window[-1]", "set -g @minus 1"),
            ("after-rename-window[a[b]", "set -g @nested yes"),
        ] {
            good(server, &["set-hook", "-g", name, value]);
        }
        good(
            server,
            &[
                "set-hook",
                "-ga",
                "after-rename-window[2]",
                "set -g @two twice",
            ],
        );
        out.push(good(
            server,
            &["show-hooks", "-gF", FIELDS, "after-rename-window"],
        ));
        good(server, &["set-hook", "-R", "after-rename-window"]);
        out.push(value(server, "@two"));
        out.push(good(
            server,
            &["show-hooks", "-gF", FIELDS, "after-rename-window[2]"],
        ));
        good(server, &["set-hook", "-gu", "after-rename-window[2]"]);
        out.push(good(
            server,
            &["show-hooks", "-gF", FIELDS, "after-rename-window"],
        ));
        good(server, &["set-hook", "-gu", "after-rename-window"]);
        out.push(good(
            server,
            &["show-hooks", "-gF", FIELDS, "after-rename-window"],
        ));
        good(
            server,
            &["set-hook", "-gT", "@custom", "set -g @custom-fired yes"],
        );
        good(server, &["set-hook", "-R", "@custom"]);
        out.push(good(server, &["show-hooks", "-gF", FIELDS, "@custom"]));
        out.push(good(server, &["show-hooks", "-gF", FIELDS, "@custom[4]"]));
        good(server, &["set-hook", "-gu", "@custom"]);
        good(server, &["set", "-g", "@custom", "ordinary again"]);
        out.push(good(server, &["show-hooks", "-gF", FIELDS]));
        out.push(good(server, &["show", "-gF", "#{option_name}"]));
        for (name, value) in [("history-limit", "1234"), ("base-index", "3")] {
            good(server, &["set-hook", "-gT", name, value]);
            out.push(good(server, &["show-hooks", "-gF", FIELDS, name]));
        }
        good(server, &["linkw", "-d", "-s", "main:0", "-t", "main:4"]);
        good(
            server,
            &[
                "set-hook",
                "-w",
                "-t",
                "main:4",
                "pane-resized",
                "set -g @pane resized",
            ],
        );
        out.push(good(server, &["show-hooks", "-w", "-t", "main:4", "-F", "#{session_name}:#{window_index}:#{window_active}:#{window_last_flag}:#{option_name}", "pane-resized"]));
        good(
            server,
            &["set-hook", "-gw", "-B", "@target:@*:#(@nothing)", ""],
        );
        out.push(good(server, &["show-hooks", "-gwB", "-t", "main:4", "-F", "#{session_name}:#{window_index}:#{window_active}:#{window_last_flag}:#{option_name}", "@target"]));
        out
    }
    let rust = contract(&Rust::new());
    let c = contract(&C::new());
    for (index, (rust, c)) in rust.iter().zip(&c).enumerate() {
        assert_eq!(
            String::from_utf8_lossy(rust),
            String::from_utf8_lossy(c),
            "observation {index}"
        );
    }
    assert_eq!(rust.len(), c.len());
}

#[test]
#[ignore = "requires current C tmux monitor hooks and real PTYs"]
fn monitor_hook_truth_filters_link_targets_and_option_lifetime_match_c() {
    fn contract(server: &impl Commands) -> Vec<Vec<u8>> {
        setup(server);
        good(server, &["set", "-g", "@gate", "0"]);
        good(server, &["set", "-g", "@seen", ""]);
        good(
            server,
            &[
                "set-hook",
                "-gT",
                "-B",
                "@truth::#{@gate}",
                "set -ag @seen '#{hook_last}>#{hook_value};'",
            ],
        );
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(value(server, "@seen"), b"\n");
        good(server, &["set", "-g", "@gate", "1"]);
        wait_value(server, "@seen", b"0>1;\n");
        good(server, &["set", "-g", "@gate", "0"]);
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(value(server, "@seen"), b"0>1;\n");
        good(server, &["set", "-g", "@gate", "2"]);
        wait_value(server, "@seen", b"0>1;0>2;\n");
        let mut out = vec![good(
            server,
            &[
                "show-hooks",
                "-gBF",
                "#{hook_fire_count}:#{>:#{hook_fire_time},0}:#{hook_monitor_target}:#{hook_monitor_format}",
                "@truth",
            ],
        )];
        out.push(good(server, &["show-hooks", "-gF", "#{hook_fire_count}:#{>:#{hook_fire_time},0}:#{hook_monitor_target}:#{hook_monitor_format}", "@truth"]));
        out.push(good(server, &["show", "-gF", "#{option_name}"]));
        out.push(good(server, &["show", "-gHF", "#{option_name}"]));
        good(server, &["set", "-gw", "@truth", "ordinary window option"]);
        out.push(good(server, &["show", "-gwF", "#{option_name}"]));
        good(
            server,
            &[
                "set",
                "-g",
                "@truth",
                "set -ag @seen 'replacement:#{hook_last}>#{hook_value};'",
            ],
        );
        good(server, &["set", "-g", "@gate", "3"]);
        wait_value(server, "@seen", b"0>1;0>2;replacement:2>3;\n");
        out.push(good(
            server,
            &["show-hooks", "-gF", "#{hook_fire_count}", "@truth"],
        ));
        good(server, &["set-hook", "-gT", "-B", "@truth::#{@gate}"]);
        out.push(good(
            server,
            &[
                "show-hooks",
                "-gBF",
                "#{hook_fire_count}:#{hook_fire_time}",
                "@truth",
            ],
        ));
        out.push(good(
            server,
            &["show-hooks", "-gF", "#{hook_fire_count}", "@truth"],
        ));
        good(server, &["set", "-gu", "@truth"]);
        good(server, &["set", "-g", "@truth", "set -g @seen unexpected"]);
        out.push(good(server, &["show-hooks", "-gB", "@truth"]));
        good(server, &["set", "-g", "@gate", "4"]);
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(value(server, "@seen"), b"0>1;0>2;replacement:2>3;\n");
        good(server, &["set", "-g", "@seen", ""]);
        good(server, &["linkw", "-d", "-s", "main:0", "-t", "main:4"]);
        good(
            server,
            &[
                "set-hook",
                "-g",
                "-B",
                "@links:@*:#{@gate}",
                "set -ag @seen '#{hook_session}:#{hook_window}:#{hook_window_index}:#{hook_pane}:#{window_index}:#{hook_value};'",
            ],
        );
        std::thread::sleep(Duration::from_millis(1100));
        good(server, &["set", "-g", "@gate", "5"]);
        wait_value(server, "@seen", b"$0:@0:0::0:5;$0:@0:4::4:5;\n");
        out.push(good(
            server,
            &[
                "show-hooks",
                "-gBF",
                "#{hook_fire_count}:#{>:#{hook_fire_time},0}",
                "@links",
            ],
        ));
        good(server, &["set-hook", "-gu", "-B", "@links"]);
        out.push(good(server, &["show-hooks", "-gB", "@links"]));
        out.push(good(
            server,
            &["show-hooks", "-gF", "#{hook_fire_count}", "@links"],
        ));
        out.push(value(server, "@links"));
        out
    }
    assert_eq!(contract(&Rust::new()), contract(&C::new()));
}
#[test]
#[ignore = "requires separate current C oracle in TMUXX_C_TMUX"]
fn lifecycle_hook_dead_payload_and_order_matches_c() {
    assert_eq!(
        lifecycle_contract(&Rust::new()),
        lifecycle_contract(&C::new())
    );
}

#[test]
#[ignore = "requires current C oracle with option format loops"]
fn option_format_loops_reflect_scoped_hook_array_updates() {
    let rust = Rust::new();
    let c = C::new();
    setup(&rust);
    setup(&c);
    for args in [
        vec![
            "set-hook",
            "-g",
            "after-rename-window[9]",
            "set -g @nine 'value with spaces'",
        ],
        vec!["set-hook", "-g", "after-rename-window[2]", "set -g @two 2"],
        vec!["set-hook", "-ga", "after-rename-window", "set -g @zero 0"],
        vec![
            "set-hook",
            "-t",
            "main",
            "after-rename-window[5]",
            "set -g @session yes",
        ],
        vec![
            "set-hook",
            "-w",
            "-t",
            "main:0",
            "after-rename-window[3]",
            "set -g @window yes",
        ],
        vec![
            "set-hook",
            "-p",
            "-t",
            "%0",
            "after-rename-window[4]",
            "set -g @pane yes",
        ],
        vec![
            "set-hook",
            "-g",
            "pane-resized[2]",
            "set -g @global-window yes",
        ],
        vec![
            "set-hook",
            "-w",
            "-t",
            "main:0",
            "pane-resized[3]",
            "set -g @window yes",
        ],
        vec![
            "set-hook",
            "-gp",
            "-t",
            "%0",
            "pane-resized[4]",
            "set -g @pane yes",
        ],
        vec![
            "set-hook",
            "-p",
            "-t",
            "%0",
            "window-renamed[5]",
            "set -g @window-only yes",
        ],
        vec!["set-hook", "-gu", "after-rename-window[2]"],
        vec!["set-hook", "-gu", "after-rename-window"],
        vec!["set-hook", "-u", "-t", "main", "after-rename-window"],
        vec!["set-hook", "-wu", "-t", "main:0", "after-rename-window"],
        vec!["set-hook", "-pu", "-t", "%0", "after-rename-window"],
        vec!["set-hook", "-wgu", "pane-resized"],
        vec!["set-hook", "-wu", "-t", "main:0", "pane-resized"],
        vec!["set-hook", "-pu", "-t", "%0", "pane-resized"],
        vec!["set-hook", "-pu", "-t", "%0", "window-renamed"],
    ] {
        assert_eq!(rust.run(&args), c.run(&args), "command {args:?}");
        for scope in ["g", "gw", "s", "w", "p"] {
            let format = format!(
                "#{{O/{scope}:#{{?#{{option_is_hook}},#{{option_name}}=#{{option_value}}:#{{option_is_array}}:#{{option_is_hook}}:#{{option_array_key}}:#{{option_array_count}}:#{{option_array_first}}:#{{option_array_last}}|,}}}}"
            );
            let query = ["display-message", "-p", "-t", "main:0", &format];
            assert_eq!(
                good(&rust, &query),
                good(&c, &query),
                "O/{scope} after {args:?}"
            );
        }
    }
}

fn creation_contract(server: &impl Commands) -> Vec<Vec<u8>> {
    setup(server);
    let mut observations = Vec::new();
    for (event, command) in [
        (
            "after-new-window",
            vec![
                "new-window",
                "-d",
                "-t",
                "main:2",
                "-n",
                "created",
                "sleep",
                "30",
            ],
        ),
        (
            "after-split-window",
            vec!["split-window", "-d", "-t", "other:0", "sleep", "30"],
        ),
        (
            "after-new-session",
            vec![
                "new-session",
                "-d",
                "-s",
                "third",
                "-n",
                "third-window",
                "sleep",
                "30",
            ],
        ),
    ] {
        good(
            server,
            &[
                "set-hook",
                "-g",
                event,
                "set -gF @creation '#{hook}|#{session_name}|#{window_name}|#{pane_id}|#{hook_pane}'",
            ],
        );
        good(server, &command);
        observations.push(value(server, "@creation"));
        good(server, &["set-hook", "-gu", event]);
    }
    observations.push(good(
        server,
        &[
            "list-windows",
            "-t",
            "main",
            "-F",
            "#{window_index}:#{window_active}",
        ],
    ));
    observations.push(good(
        server,
        &[
            "list-panes",
            "-t",
            "other:0",
            "-F",
            "#{pane_id}:#{pane_active}",
        ],
    ));
    observations
}
#[test]
fn creation_after_hooks_target_created_detached_objects_without_selecting_them() {
    let out = creation_contract(&Rust::new());
    assert_eq!(out[0], b"after-new-window|main|created|%2|\n");
    assert_eq!(out[1], b"after-split-window|other|second|%3|\n");
    assert_eq!(out[2], b"after-new-session|third|third-window|%4|\n");
    assert_eq!(out[3], b"0:1\n2:0\n");
    assert_eq!(out[4], b"%1:1\n%3:0\n");
}
#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn creation_after_hooks_and_detached_selection_match_c() {
    assert_eq!(
        creation_contract(&Rust::new()),
        creation_contract(&C::new())
    );
}

fn client_hooks_setup(server: &impl Commands) {
    setup(server);
    good(server, &["set", "-g", "@clients", ""]);
    for event in [
        "client-created",
        "client-attached",
        "client-session-changed",
        "client-resized",
        "client-detached",
        "client-closed",
    ] {
        good(
            server,
            &[
                "set-hook",
                "-g",
                event,
                "set -gF @clients '#{@clients}|#{hook}:#{hook_session_name}:#{hook_old_session_name}:#{hook_new_session_name}:#{hook_width}:#{hook_height}'",
            ],
        );
    }
}
fn native_control(client: &LocalClient, args: &[&str]) {
    client
        .send(Request {
            request_id: 91,
            argv: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
            stdin: None,
            subscribe: false,
            control: true,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{args:?}");
            return;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn rust_client_contract() -> Vec<u8> {
    let server = Rust::new();
    client_hooks_setup(&server);
    let client = server.host.connect().unwrap();
    native_control(&client, &["attach-session", "-t", "main"]);
    native_control(&client, &["switch-client", "-t", "other"]);
    native_control(&client, &["refresh-client", "-C", "77,22"]);
    native_control(&client, &["detach-client"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let log = value(&server, "@clients");
        if log
            .windows(b"client-closed".len())
            .any(|s| s == b"client-closed")
        {
            return log;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            String::from_utf8_lossy(&log)
        );
    }
}
#[test]
fn client_lifecycle_hooks_follow_control_attach_switch_resize_and_detach() {
    let log = rust_client_contract();
    let log = String::from_utf8(log).unwrap();
    assert!(
        log.contains("|client-session-changed:main::main::"),
        "{log}"
    );
    assert!(log.contains("|client-attached:main::::"), "{log}");
    assert!(
        log.contains("|client-session-changed:other:main:other::"),
        "{log}"
    );
    assert!(
        !log.contains("|client-resized:"),
        "control -C sizing does not fire terminal resize hooks: {log}"
    );
    assert!(
        log.ends_with("|client-detached:other::::|client-closed:other::::\n"),
        "{log}"
    );
}
#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn client_lifecycle_attach_switch_resize_detach_matches_c() {
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::process::{Command, Stdio};
    let server = C::new();
    client_hooks_setup(&server);
    #[cfg(unix)]
    let mut child = Command::new(&server.program)
        .arg("-S")
        .arg(&server.socket)
        .args(["-f", "/dev/null", "-C", "attach-session", "-t", "main"])
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    #[cfg(windows)]
    let child = server.remote.control_open("main");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let log = value(&server, "@clients");
        if log
            .windows(b"client-attached".len())
            .any(|s| s == b"client-attached")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            String::from_utf8_lossy(&log)
        );
    }
    #[cfg(unix)]
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"switch-client -t other\nrefresh-client -C 77,22\ndetach-client\n")
        .unwrap();
    #[cfg(windows)]
    {
        let output =
            child.write(b"switch-client -t other\nrefresh-client -C 77,22\ndetach-client\n");
        assert!(output.status.success(), "{output:?}");
        while child.exited().is_none() {
            assert!(Instant::now() < deadline, "C control child timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(child.exited(), Some(0));
    }
    #[cfg(unix)]
    {
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("control child timed out");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let c = value(&server, "@clients");
    assert_eq!(
        String::from_utf8(rust_client_contract()).unwrap(),
        String::from_utf8(c).unwrap()
    );
}

#[test]
fn readonly_attach_blocks_commands_and_input_until_switch_toggle() {
    let server = Rust::new();
    setup(&server);
    let client = server.host.connect().unwrap();
    native_control(&client, &["attach-session", "-r", "-t", "main"]);
    for args in [
        vec!["display-message", "-p", "blocked"],
        vec!["send-keys", "-t", "main:0", "bad"],
    ] {
        client
            .send(Request {
                request_id: 92,
                argv: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
                stdin: None,
                subscribe: false,
                control: true,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(ServerMessage::Reply(response)) = client.try_recv() {
                assert_eq!(response.error.as_deref(), Some("client is read-only"));
                break;
            }
            assert!(Instant::now() < deadline);
        }
    }
    native_control(&client, &["switch-client", "-r", "-t", "other"]);
    native_control(&client, &["set-option", "-g", "@unlocked", "yes"]);
    assert_eq!(value(&server, "@unlocked"), b"yes\n");
    native_control(
        &client,
        &["attach-session", "-f", "read-only,!read-only", "-t", "main"],
    );
    native_control(&client, &["detach-client"]);
}

#[test]
fn embedded_control_rejects_ambiguous_source_before_owner_mutation() {
    let server = Rust::new();
    good(&server, &["set", "-g", "@source-guard", "before"]);
    let error = server
        .client
        .send_control(tmuxx_protocol::native::ControlRequest {
            request_id: 93,
            argv: vec![
                b"set".to_vec(),
                b"-g".to_vec(),
                b"@source-guard".to_vec(),
                b"argv".to_vec(),
            ],
            source: Some(b"set -g @source-guard source".to_vec()),
            subscribe: false,
        })
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(value(&server, "@source-guard"), b"before\n");
}

#[test]
#[ignore = "requires current C oracle with client format loops"]
fn client_format_loops_follow_attached_clients_and_preserve_target_defaults() {
    #[cfg(unix)]
    use std::{
        io::Write,
        process::{Child, Command, Stdio},
    };
    #[cfg(unix)]
    struct Controls(Vec<Child>);
    #[cfg(unix)]
    impl Drop for Controls {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let rust = Rust::new();
    let c = C::new();
    setup(&rust);
    setup(&c);
    let clients = [rust.host.connect().unwrap(), rust.host.connect().unwrap()];
    #[cfg(unix)]
    let mut controls = Controls(Vec::new());
    #[cfg(windows)]
    let mut controls = Vec::new();
    for (index, session) in ["main", "other"].into_iter().enumerate() {
        native_control(&clients[index], &["attach-session", "-t", session]);
        #[cfg(unix)]
        controls.0.push(
            Command::new(&c.program)
                .arg("-S")
                .arg(&c.socket)
                .args(["-f", "/dev/null", "-C", "attach-session", "-t", session])
                .env_remove("TMUX")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        #[cfg(windows)]
        controls.push(c.remote.control_open(session));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while good(&c, &["list-clients", "-F", "#{client_session}"])
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .count()
        != 2
    {
        assert!(
            Instant::now() < deadline,
            "C control clients did not attach"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    for format in [
        "#{L:#{client_session}:#{client_control_mode}:#{session_name}:#{loop_index}:#{loop_last_flag}|}",
        "#{L/nr:#{client_session}:#{session_name}:#{loop_index}:#{loop_last_flag}|}",
        "#{L/ir:#{client_session}|}",
        "#{n;L:#{client_session}}",
        "#{S:#{session_name}[#{L:#{client_session}:#{session_name}|}]}",
    ] {
        let args = ["display-message", "-p", "-t", "main:0", format];
        assert_eq!(good(&rust, &args), good(&c, &args), "{format}");
    }
    native_control(&clients[0], &["detach-client"]);
    #[cfg(unix)]
    controls.0[0]
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"detach-client\n")
        .unwrap();
    #[cfg(windows)]
    assert!(controls[0].write(b"detach-client\n").status.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    #[cfg(unix)]
    while controls.0[0].try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "C control client did not detach");
        std::thread::sleep(Duration::from_millis(2));
    }
    #[cfg(windows)]
    while controls[0].exited().is_none() {
        assert!(Instant::now() < deadline, "C control client did not detach");
        std::thread::sleep(Duration::from_millis(2));
    }
    let args = [
        "display-message",
        "-p",
        "-t",
        "main:0",
        "#{L:#{client_session}:#{loop_index}:#{loop_last_flag}|}",
    ];
    assert_eq!(good(&rust, &args), good(&c, &args));
}
