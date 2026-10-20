#![cfg(any(unix, windows))]
#[cfg(windows)]
#[path = "support/windows_fixture.rs"]
mod windows_fixture;
use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Request, Response, ServerMessage};
use tmuxx_runtime::LocalClient;

fn run(client: &LocalClient, argv: &[&str]) -> Response {
    #[cfg(unix)]
    let args = argv.iter().map(|arg| arg.as_bytes().to_vec()).collect();
    #[cfg(windows)]
    let args = windows_fixture::argv(argv);
    client
        .send(Request {
            request_id: 1,
            argv: args,
            stdin: None,
            subscribe: true,
            control: false,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            return reply;
        }
        assert!(
            Instant::now() < deadline,
            "native UI request blocked the command queue: {argv:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn state(client: &LocalClient) -> Vec<Vec<u8>> {
    [
        vec!["list-panes","-a","-F","#{session_id}:#{window_id}:#{pane_id}:#{pane_pid}:#{pane_width}:#{pane_height}:#{pane_active}"],
        vec!["show-buffer","-b","retained"], vec!["capture-pane","-p"],
        vec!["show-options","-g"], vec!["show-options","-s"], vec!["show-options","-wg"], vec!["show-options","-p"],
    ].into_iter().map(|args|{let reply=run(client,&args);assert_eq!(reply.error,None);reply.output}).collect()
}

#[test]
fn every_scoped_native_ui_command_rejects_without_state_or_input_mutation() {
    #[cfg(unix)]
    let path = std::env::temp_dir().join(format!("tmuxx-ui-policy-{}.sock", std::process::id()));
    #[cfg(windows)]
    let path =
        tmuxx_runtime::windows::endpoint(&format!("ui-policy-{}", std::process::id())).unwrap();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(path)).unwrap();
    let client = host.connect().unwrap();
    assert_eq!(
        run(
            &client,
            &[
                "new",
                "-d",
                "-s",
                "policy",
                "/bin/sh",
                "-c",
                "stty raw -echo; printf READY; exec cat"
            ]
        )
        .error,
        None
    );
    assert_eq!(
        run(&client, &["set-buffer", "-b", "retained", "binary-buffer"]).error,
        None
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !run(&client, &["capture-pane", "-p"])
        .output
        .starts_with(b"READY")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let commands: &[(&str, &[&str])] = &[
        ("bind-key", &["F1", "set -g @mutated yes"]),
        ("unbind-key", &["F1"]),
        ("list-keys", &[]),
        ("choose-tree", &[]),
        ("choose-client", &[]),
        ("choose-buffer", &[]),
        ("customize-mode", &[]),
        ("switch-mode", &[]),
        ("display-panes", &["set -g @mutated yes"]),
        ("command-prompt", &["set -g @mutated yes"]),
        ("confirm-before", &["set -g @mutated yes"]),
        ("copy-mode", &[]),
        ("clock-mode", &[]),
        ("display-menu", &["mutate", "m", "set -g @mutated yes"]),
        ("display-popup", &["printf MUTATED"]),
        ("find-window", &["policy"]),
        ("lock-server", &[]),
        ("lock-session", &[]),
        ("lock-client", &[]),
        ("suspend-client", &[]),
        ("show-prompt-history", &[]),
        ("clear-prompt-history", &[]),
    ];
    let scope: serde_json::Value =
        serde_json::from_str(include_str!("../../../conformance/scope.json")).unwrap();
    let expected = scope["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["id"] == "native-ui-policy")
        .unwrap()["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        commands
            .iter()
            .map(|(name, _)| *name)
            .collect::<std::collections::BTreeSet<_>>(),
        expected,
        "new native UI commands need explicit policy fixtures"
    );
    let before = state(&client);
    for (name, args) in commands {
        let mut argv = vec![*name];
        argv.extend_from_slice(args);
        let reply = run(&client, &argv);
        assert!(
            reply
                .error
                .as_deref()
                .is_some_and(|error| error.contains("unsupported")),
            "{argv:?}: {reply:?}"
        );
        assert_eq!(
            state(&client),
            before,
            "unsupported {name} changed retained state"
        );
        assert_eq!(
            run(&client, &["display", "-p", "barrier"]).output,
            b"barrier\n"
        );
    }
    for args in [
        vec!["send-keys", "-X", "copy-selection-and-cancel"],
        vec!["send-keys", "-K", "F1"],
        vec!["select-pane", "-P", "dim=50%"],
        vec!["refresh-client", "-l"],
        vec!["detach-client", "-E", "printf MUTATED"],
        vec!["split-window", "-m", "exited"],
        vec!["new-pane", "-m", "exited"],
        vec!["split-window", "-S", "fg=red"],
        vec!["new-pane", "-R", "fg=blue"],
        vec!["split-window", "-s", "bold"],
        vec!["new-pane", "-s", "bg=terminal"],
        vec!["split-window", "-s", "fg=invalid"],
        vec!["new-pane", "-s", "bg=#zzzzzz"],
    ] {
        let reply = run(&client, &args);
        assert!(
            reply
                .error
                .as_deref()
                .is_some_and(|error| error.contains("unsupported")),
            "{args:?}: {reply:?}"
        );
        assert_eq!(
            state(&client),
            before,
            "unsupported UI flag changed retained state"
        );
    }
    assert_eq!(
        run(&client, &["send-keys", "-l", "INPUT-STILL-LIVE"]).error,
        None
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !String::from_utf8_lossy(&run(&client, &["capture-pane", "-p"]).output)
        .contains("INPUT-STILL-LIVE")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    host.shutdown().unwrap();
}

#[test]
fn control_popup_rejection_preserves_pane_and_next_guarded_reply() {
    #[cfg(unix)]
    let path = format!("/tmp/tx-popup-{}", std::process::id());
    #[cfg(windows)]
    let path = tmuxx_runtime::windows::endpoint(&format!("popup-{}", std::process::id())).unwrap();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(path)).unwrap();
    let admin = host.connect().unwrap();
    assert_eq!(
        run(&admin, &["new", "-d", "-s", "popup", "sleep 30"]).error,
        None
    );
    let control = host.connect().unwrap();
    let query = |id, args: &[&str]| {
        control
            .send(Request {
                request_id: id,
                argv: args.iter().map(|a| a.as_bytes().to_vec()).collect(),
                stdin: None,
                subscribe: false,
                control: true,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(ServerMessage::Reply(reply)) = control.try_recv() {
                assert_eq!(reply.request_id, id);
                break reply;
            }
            assert!(
                Instant::now() < deadline,
                "control popup blocked subsequent reply"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    assert_eq!(query(1, &["attach-session", "-t", "popup"]).error, None);
    assert_eq!(
        run(&admin, &["set-buffer", "-b", "retained", "sentinel"]).error,
        None
    );
    let before = state(&admin);
    assert!(
        query(2, &["display-popup", "set -g @mutated yes"])
            .error
            .unwrap()
            .contains("unsupported")
    );
    assert_eq!(query(3, &["display", "-p", "barrier"]).output, b"barrier\n");
    assert_eq!(state(&admin), before);
    host.shutdown().unwrap();
}
