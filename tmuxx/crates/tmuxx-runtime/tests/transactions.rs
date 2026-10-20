#![cfg(any(unix, windows))]

use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Request, Response, ServerMessage};
use tmuxx_runtime::{LocalClient, Options};

fn run(client: &LocalClient, argv: &[&str]) -> Response {
    client
        .send(Request {
            request_id: 1,
            argv: argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            subscribe: false,
            control: false,
            stdin: None,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            return reply;
        }
        assert!(Instant::now() < deadline, "timed out waiting for reply");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn failed_spawn_does_not_leave_a_session_or_poison_next_process() {
    #[cfg(unix)]
    let path = std::env::temp_dir().join(format!("tmuxx-transaction-{}.sock", std::process::id()));
    #[cfg(windows)]
    let path =
        tmuxx_runtime::windows::endpoint(&format!("transaction-{}", std::process::id())).unwrap();
    let host = tmuxx_runtime::start(Options::new(&path)).unwrap();
    let client = host.connect().unwrap();
    let reply = run(
        &client,
        &[
            "new-session",
            "-d",
            "-s",
            "failed",
            "/tmuxx-nonexistent-executable",
            "argument",
        ],
    );
    assert!(reply.error.is_some());
    assert!(
        run(&client, &["list-sessions", "-F", "#{session_name}"])
            .output
            .is_empty()
    );
    assert_eq!(
        run(
            &client,
            &[
                "new-session",
                "-d",
                "-s",
                "working",
                if cfg!(windows) { "python" } else { "/bin/sh" },
                "-c",
                if cfg!(windows) {
                    "import time; print('READY',flush=True); time.sleep(30)"
                } else {
                    "printf READY; sleep 30"
                }
            ]
        )
        .error,
        None
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let capture = run(&client, &["capture-pane", "-p"]);
        if capture.output.starts_with(b"READY") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "replacement process did not run: {capture:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let graph = [
        "list-panes",
        "-a",
        "-F",
        "#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_width}|#{pane_height}|#{window_layout}",
    ];
    let before = run(&client, &graph).output;
    for command in ["new-pane", "newp", "new-p", "split-window", "new-window"] {
        let reply = run(
            &client,
            &[command, "-d", "/tmuxx-nonexistent-executable", "argument"],
        );
        assert!(reply.error.is_some(), "{command}");
        assert_eq!(
            run(&client, &graph).output,
            before,
            "{command} left a partial pane"
        );
        assert!(
            run(&client, &["capture-pane", "-p", "-t", "%0"])
                .output
                .starts_with(b"READY")
        );
    }
    let next = run(&client, &["new-pane", "-dEP", "-F", "#{pane_id}"]);
    assert_eq!(next.error, None);
    assert_eq!(next.output, b"%1\n", "failed spawns consumed pane IDs");
    host.shutdown().unwrap();
}

#[test]
fn nested_source_keeps_preparation_before_execution_and_group_errors_stop_queue() {
    let dir = std::env::temp_dir().join(format!("tmuxx-source-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let inner = dir.join("inner.conf");
    let outer = dir.join("outer.conf");
    std::fs::write(&inner, "set -g @value inner\n").unwrap();
    std::fs::write(
        &outer,
        format!(
            "source-file {}\ndisplay -p '#{{@value}}'\n",
            String::from_utf8(tmuxx_command::quote_argument(
                inner.to_str().unwrap().as_bytes()
            ))
            .unwrap()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    let endpoint = dir.join("socket");
    #[cfg(windows)]
    let endpoint =
        tmuxx_runtime::windows::endpoint(&format!("source-{}", std::process::id())).unwrap();
    let host = tmuxx_runtime::start(Options::new(endpoint)).unwrap();
    let client = host.connect().unwrap();
    assert_eq!(
        run(&client, &["source-file", outer.to_str().unwrap()]).output,
        b"inner\n"
    );
    let failed = run(
        &client,
        &[
            "set",
            "-g",
            "@value",
            "before",
            ";",
            "missing-command",
            ";",
            "set",
            "-g",
            "@value",
            "after",
        ],
    );
    assert!(failed.error.is_some());
    assert_eq!(run(&client, &["show", "-gv", "@value"]).output, b"inner\n");
    let failed = run(
        &client,
        &[
            "set",
            "-g",
            "@value",
            "before",
            ";",
            "rename-session",
            "-t",
            "missing-session",
            "unused",
            ";",
            "set",
            "-g",
            "@value",
            "after",
        ],
    );
    assert!(failed.error.is_some());
    assert_eq!(run(&client, &["show", "-gv", "@value"]).output, b"before\n");
    host.shutdown().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
