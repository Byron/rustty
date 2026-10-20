#![cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote;
use remote::{RemoteControl, RemoteReference};
use std::time::{Duration, Instant};

fn wait(control: &RemoteControl) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while control.exited().is_none() {
        assert!(Instant::now() < deadline, "remote command did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(control.exited(), Some(0));
}

#[test]
#[ignore = "requires explicit pinned TMUXX_REMOTE_C_CONFIG"]
fn implicit_configuration_starter_loss_releases_rejected_clients_and_waiters() {
    let reference = RemoteReference::new_with_configuration(
        b"set -ga @runs x\nnew-session -d -s keep 'sleep 120'\nrun-shell -b 'touch background'\nrun-shell 'touch ready; while [ ! -f go ]; do sleep .01; done'\nsource-file /dev/null\nset -g @late 1\n",
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while !reference.exists(b"ready") || !reference.exists(b"background") {
        assert!(
            Instant::now() < deadline,
            "startup configuration did not reach its barrier"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let starter = reference.startup_client();
    assert!(starter.exited().is_none());
    starter.close();
    assert!(reference.server_alive());
    let lost = reference.command_open_args(&[b"display", b"-p", b"lost"]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(lost.exited().is_none());
    lost.close();
    reference.malformed_startup_clients();
    let waiting = reference.command_open_args(&[
        b"new-session",
        b"-d",
        b"-s",
        b"second",
        b"sleep 120",
        b";",
        b"display",
        b"-p",
        b"#{@runs} #{@late}",
    ]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(waiting.exited().is_none());
    reference.put(b"go", b"");
    wait(&waiting);
    assert_eq!(waiting.read().stdout, b"x 1\n");
    waiting.close();
    assert_eq!(
        reference
            .output(&[b"list-sessions", b"-F", b"#{session_name}"], &[])
            .stdout,
        b"keep\nsecond\n"
    );
    assert_eq!(
        reference
            .output(&[b"display", b"-p", b"#{@runs} #{@late}"], &[])
            .stdout,
        b"x 1\n"
    );
}

#[test]
#[ignore = "requires explicit pinned TMUXX_REMOTE_C_CONFIG"]
fn persistent_controls_held_stdin_environment_and_pidfd_cleanup() {
    let reference = RemoteReference::new_with_environment(&[
        (b"TMUXX_ORACLE_START", Some("雪 & value".as_bytes())),
        (b"EDITOR", None),
    ]);
    assert!(
        reference
            .output(
                &[b"new-session", b"-d", b"-s", b"main", b"sleep", b"30"],
                &[]
            )
            .status
            .success()
    );
    assert_eq!(
        reference
            .output(&[b"show-environment", b"-g", b"TMUXX_ORACLE_START"], &[])
            .stdout,
        "TMUXX_ORACLE_START=雪 & value\n".as_bytes()
    );
    let changed = reference.output_with_environment(
        &[
            b"display-message",
            b"-p",
            b"-t",
            b"main",
            b"#{V/c:#{?#{m:TMUXX_ORACLE_REQUEST,#{environ_name}},#{environ_value},}}",
        ],
        &[],
        &[(b"TMUXX_ORACLE_REQUEST", Some(b"request-only"))],
    );
    assert!(changed.status.success(), "{changed:?}");
    assert_eq!(changed.stdout, b"request-only\n");

    let source = reference.command_open_args(&[b"source-file", b"-"]);
    assert!(source.write(b"set -g @held ").status.success());
    assert!(
        reference
            .output(&[b"display-message", b"-p", b"independent"], &[])
            .status
            .success()
    );
    assert!(source.write(b"yes\n").status.success());
    assert!(source.finish_input().status.success());
    wait(&source);
    assert_eq!(
        reference
            .output(&[b"show-options", b"-gqv", b"@held"], &[])
            .stdout,
        b"yes\n"
    );
    assert!(source.read().stdout.is_empty());
    source.close();

    let waiter = reference.command_open_args(&[
        b"wait-for",
        b"oracle-release",
        b";",
        b"display-message",
        b"-p",
        b"flushed",
    ]);
    assert!(waiter.exited().is_none());
    assert!(
        reference
            .output(&[b"wait-for", b"-S", b"oracle-release"], &[])
            .status
            .success()
    );
    wait(&waiter);
    assert_eq!(waiter.read().stdout, b"flushed\n");
    waiter.close();

    let control = reference.control_open("main");
    assert!(!control.name.is_empty());
    let watch = reference.watch_process(control.pid);
    assert!(reference.process_alive(watch));
    assert!(control.read().status.success());
    assert!(
        control
            .write(b"display -p retained\ndetach-client\n")
            .status
            .success()
    );
    let mut bytes = Vec::new();
    loop {
        let output = control.read();
        assert!(output.status.success(), "{output:?}");
        if output.stdout.is_empty() {
            break;
        }
        bytes.extend(output.stdout);
    }
    assert!(bytes.windows(8).any(|part| part == b"retained"));
    assert!(bytes.windows(5).any(|part| part == b"%exit"));
    wait(&control);
    assert!(!reference.process_alive(watch));
    assert!(reference.server_alive());
    control.close();

    let retained = reference.control_open("main");
    drop(reference); // The owned client keeps this exact fixture alive.
    assert!(retained.read().status.success());
    assert!(retained.write(b"display -p still-owned\n").status.success());
    let mut bytes = Vec::new();
    while !bytes.windows(11).any(|part| part == b"still-owned") {
        let output = retained.read();
        assert!(output.status.success() && !output.stdout.is_empty());
        bytes.extend(output.stdout);
    }
}
