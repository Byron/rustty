#![cfg(any(unix, windows))]

#[cfg(windows)]
#[path = "support/windows_fixture.rs"]
mod windows_fixture;

use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Request, Response, ServerMessage};
use tmuxx_runtime::{LocalClient, Options};

fn run(client: &LocalClient, argv: &[&str]) -> Response {
    #[cfg(unix)]
    let args = argv.iter().map(|arg| arg.as_bytes().to_vec()).collect();
    #[cfg(windows)]
    let args = windows_fixture::argv(argv);
    client
        .send(Request {
            request_id: 1,
            argv: args,
            subscribe: false,
            control: false,
            stdin: None,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{argv:?}");
            return reply;
        }
        assert!(Instant::now() < deadline, "timed out waiting for reply");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn output_contains(client: &LocalClient, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = run(client, &["capture-pane", "-p", "-t", "%0"]).output;
        if String::from_utf8_lossy(&output).contains(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "missing {expected:?} in {output:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn pane_moves_keep_the_same_child_and_resize_its_pty() {
    #[cfg(unix)]
    let path = std::env::temp_dir().join(format!("tmuxx-moves-{}.sock", std::process::id()));
    #[cfg(windows)]
    let path = tmuxx_runtime::windows::endpoint(&format!("moves-{}", std::process::id())).unwrap();
    let host = tmuxx_runtime::start(Options::new(&path)).unwrap();
    let client = host.connect().unwrap();
    let script = "import os,tty\ntty.setraw(0)\nos.write(1,b'READY')\ni=0\nwhile os.read(0,1):\n i+=1; s=os.get_terminal_size(0); os.write(1,f'\\r\\n{i}:{s.columns}x{s.lines}'.encode())";
    run(
        &client,
        &[
            "new-session",
            "-d",
            "-s",
            "main",
            "python3",
            "-u",
            "-c",
            script,
        ],
    );
    output_contains(&client, "READY");
    let pid = run(&client, &["display", "-pt", "%0", "#{pane_pid}"]).output;
    for (index, args) in [
        vec!["splitw", "-h", "-t", "%0", "sleep", "30"],
        vec!["neww", "-d", "-t", "main:1", "sleep", "30"],
        vec!["swapp", "-s", "%0", "-t", "%2"],
        vec!["joinp", "-h", "-s", "%0", "-t", "%1"],
        vec!["rotatew", "-D", "-t", "main:0"],
        vec!["breakp", "-s", "%0", "-t", "main:7"],
        vec!["set", "-w", "-t", "%0", "pane-border-status", "top"],
        vec!["set", "-w", "-t", "%0", "pane-border-status", "bottom"],
        vec!["set", "-w", "-t", "%0", "pane-border-status", "off"],
    ]
    .iter()
    .enumerate()
    {
        run(&client, args);
        assert_eq!(
            run(&client, &["display", "-pt", "%0", "#{pane_pid}"]).output,
            pid
        );
        let size = run(
            &client,
            &["display", "-pt", "%0", "#{pane_width}x#{pane_height}"],
        )
        .output;
        run(&client, &["send-keys", "-t", "%0", "-l", "q"]);
        output_contains(
            &client,
            &format!("{}:{}", index + 1, String::from_utf8_lossy(&size).trim()),
        );
    }
    host.shutdown().unwrap();
}

#[test]
fn reconnected_native_view_keeps_zoomed_child_and_modal_cancel_restores_focus() {
    #[cfg(unix)]
    let path = std::env::temp_dir().join(format!("tmuxx-modal-{}.sock", std::process::id()));
    #[cfg(windows)]
    let path = tmuxx_runtime::windows::endpoint(&format!("modal-{}", std::process::id())).unwrap();
    let host = tmuxx_runtime::start(Options::new(path)).unwrap();
    let admin = host.connect().unwrap();
    run(
        &admin,
        &[
            "new-session",
            "-d",
            "-s",
            "main",
            "-x",
            "80",
            "-y",
            "24",
            "sleep",
            "30",
        ],
    );
    run(&admin, &["splitw", "-dh", "-t", "%0", "sleep", "30"]);
    let pid = run(&admin, &["display", "-pt", "%0", "#{pane_pid}"]).output;
    run(&admin, &["resizep", "-Z", "-t", "%0"]);
    run(
        &admin,
        &[
            "newp",
            "-OCDK",
            "-t",
            "%0",
            "-x",
            "20",
            "-y",
            "10",
            "-X",
            "-3",
            "-T",
            "modal-#{pane_id}",
            "sleep",
            "30",
        ],
    );
    let view = host
        .connect_with_context(tmuxx_core::ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    view.send(Request {
        request_id: 2,
        argv: vec![b"attach-session".to_vec(), b"-t".to_vec(), b"main".to_vec()],
        subscribe: true,
        control: false,
        stdin: None,
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let state = loop {
        if let Ok(ServerMessage::Snapshot { state, .. }) = view.try_recv() {
            break state;
        }
        assert!(Instant::now() < deadline, "missing reconnect snapshot");
        std::thread::sleep(Duration::from_millis(2));
    };
    let window = &state["windows"][0];
    assert_eq!(window["zoomed_pane"], 0);
    assert_eq!(window["active"], 2);
    let layout: serde_json::Value =
        serde_json::from_str(window["visible_layout"].as_str().unwrap()).unwrap();
    assert_eq!(layout["L"]["c"].as_array().unwrap().len(), 2);
    assert_eq!(state["panes"][0]["cols"], 80);
    let modal = &state["panes"][2];
    assert_eq!(modal["x"], -2);
    assert_eq!(modal["cols"], 18);
    for flag in [
        "modal",
        "modal_capture_keys",
        "modal_close_click",
        "modal_close_cancel",
    ] {
        assert_eq!(modal[flag], true);
    }
    let event = tmuxx_terminal::Input::Key {
        event: tmuxx_terminal::KeyEvent {
            key: tmuxx_terminal::Key::Escape,
            text: None,
            modifiers: Default::default(),
            consumed_modifiers: Default::default(),
            action: tmuxx_terminal::KeyAction::Press,
            unshifted: None,
            composing: false,
        },
        options: Default::default(),
    };
    view.send(Request {
        request_id: 3,
        argv: vec![b"tmuxx-input".to_vec(), b"-t".to_vec(), b"%2".to_vec()],
        stdin: Some(serde_json::to_vec(&event).unwrap()),
        subscribe: false,
        control: false,
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = view.try_recv()
            && reply.request_id == 3
        {
            assert_eq!(reply.error, None);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "modal cancellation did not complete"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        run(&admin, &["list-panes", "-a", "-F", "#{pane_id}"]).output,
        b"%0\n%1\n"
    );
    assert_eq!(
        run(&admin, &["display", "-pt", "%0", "#{pane_pid}"]).output,
        pid
    );
    assert_eq!(
        run(
            &admin,
            &[
                "display",
                "-pt",
                "%0",
                "#{pane_active}:#{pane_zoomed_flag}:#{window_zoomed_flag}"
            ]
        )
        .output,
        b"1:1:1\n"
    );
    host.shutdown().unwrap();
}
