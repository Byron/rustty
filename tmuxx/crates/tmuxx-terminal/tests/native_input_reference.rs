#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use rustty_vt::{MouseAction, MouseButton, MouseEncodeOptions, MouseEvent};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::{path::PathBuf, process::Command};
use tmuxx_terminal::{Input, Modifiers, Terminal};

#[test]
#[ignore = "requires C tmux with attached PTY clients"]
fn native_pointer_focus_and_paste_bytes_match_c_application_modes() {
    #[cfg(windows)]
    let result = remote_reference::RemoteReference::new().observe("native_input", &[]);
    #[cfg(unix)]
    let result = {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_owned();
        Command::new("python3")
            .arg("-B")
            .arg(root.join("tmuxx/conformance/native_input.py"))
            .arg(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"))
            .output()
            .unwrap()
    };
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let observations: BTreeMap<String, String> = serde_json::from_slice(&result.stdout).unwrap();
    let check = |name: &str, bytes: Vec<u8>| {
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(hex, observations[name], "{name}");
    };
    for (name, mode) in [
        ("disabled", b"".as_slice()),
        ("normal", b"\x1b[?1000h"),
        ("drag", b"\x1b[?1002h"),
        ("all", b"\x1b[?1003h"),
        ("utf8", b"\x1b[?1000h\x1b[?1005h"),
        ("sgr", b"\x1b[?1000h\x1b[?1006h"),
        ("sgr-drag", b"\x1b[?1002h\x1b[?1006h"),
        ("sgr-all", b"\x1b[?1003h\x1b[?1006h"),
    ] {
        let mut terminal = Terminal::new(80, 24, 2000);
        terminal.feed(mode);
        let terminal = terminal.snapshot().restore().unwrap().into_backend();
        for (event, action, button, col) in [
            ("press", MouseAction::Press, Some(MouseButton::Left), 2),
            ("drag", MouseAction::Move, Some(MouseButton::Left), 3),
            ("release", MouseAction::Release, Some(MouseButton::Left), 3),
            ("move", MouseAction::Move, None, 4),
            ("wheel", MouseAction::Press, Some(MouseButton::WheelUp), 4),
            ("modified", MouseAction::Press, Some(MouseButton::Left), 5),
        ] {
            let bytes = terminal.encode_mouse(
                MouseEvent {
                    action,
                    button,
                    x: f64::from(col * 10),
                    y: 10.0,
                    modifiers: Modifiers {
                        alt: event == "modified",
                        control: event == "modified",
                        ..Modifiers::default()
                    },
                },
                MouseEncodeOptions {
                    screen_size: [800.0, 240.0],
                    cell_size: [10, 10],
                    padding: [0.0; 4],
                    any_button_pressed: button == Some(MouseButton::Left),
                    last_cell: None,
                },
            );
            check(&format!("{name}:{event}"), bytes);
        }
    }
    for enabled in [false, true] {
        let mut terminal = Terminal::new(80, 24, 2000);
        if enabled {
            terminal.feed(b"\x1b[?1004h");
        }
        // The frontend emits focus only when subscribed. C still forwards
        // explicitly injected focus when focus-events is disabled upstream.
        let terminal = terminal.snapshot().restore().unwrap().into_backend();
        let mut bytes = terminal.encode_focus(false);
        bytes.extend(terminal.encode_focus(true));
        check(&format!("focus:on:{}", u8::from(enabled)), bytes.clone());
        check(&format!("focus:off:{}", u8::from(enabled)), bytes);
        let mut terminal = Terminal::new(80, 24, 2000);
        if enabled {
            terminal.feed(b"\x1b[?2004h");
        }
        check(
            &format!("paste:{}", u8::from(enabled)),
            terminal.encode_input(&Input::Paste(b"a\nb\rc\t".to_vec())),
        );
    }
    assert_eq!(observations.len(), 54);
}
