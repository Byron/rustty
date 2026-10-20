#![cfg(any(unix, windows))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tmuxx_core::ClientContext;
use tmuxx_protocol::native::{Request, Response, ServerMessage};
use tmuxx_runtime::{LocalClient, Options, ServerHandle};

fn host() -> ServerHandle {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[cfg(unix)]
    let path = std::env::temp_dir().join(format!(
        "tmuxx-views-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(windows)]
    let path = tmuxx_runtime::windows::endpoint(&format!(
        "native-views-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap();
    tmuxx_runtime::start(Options::new(path)).unwrap()
}

#[cfg(windows)]
const RAW_CONSOLE: &str = "import ctypes,msvcrt,os\nk=ctypes.WinDLL('kernel32',use_last_error=True)\nk.GetStdHandle.restype=ctypes.c_void_p\nk.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]\nassert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)\nassert k.SetConsoleMode(k.GetStdHandle(-10),0x200)\nassert k.SetConsoleMode(k.GetStdHandle(-11),0xD)\nmsvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)\n";

fn fixture_argv(argv: &[&str]) -> Vec<Vec<u8>> {
    #[cfg(unix)]
    let argv = argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    #[cfg(windows)]
    let argv = {
        let mut values = argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        // These are explicit fixture programs, not command-runtime rewrites.
        if values.len() >= 2 && values[values.len() - 2..] == ["sleep", "30"] {
            values.truncate(values.len() - 2);
            values.extend(["python", "-u", "-c", "import time; time.sleep(30)"].map(str::to_owned));
        }
        if let Some(index) = values.iter().position(|arg| arg == "python3") {
            values[index] = "python".into();
            let script = values.last_mut().unwrap();
            *script = script
                .replace("import os,tty; tty.setraw(0); ", RAW_CONSOLE)
                .replace("import os,tty\ntty.setraw(0)\n", RAW_CONSOLE);
        }
        values
    };
    argv.iter().map(|arg| arg.as_bytes().to_vec()).collect()
}

fn receive<T>(client: &LocalClient, mut select: impl FnMut(ServerMessage) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(message) = client.try_recv()
            && let Some(value) = select(message)
        {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out receiving native event"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn command(
    client: &LocalClient,
    argv: &[&str],
    input: Option<Vec<u8>>,
    subscribe: bool,
) -> Response {
    client
        .send(Request {
            request_id: 1,
            argv: fixture_argv(argv),
            stdin: input,
            subscribe,
            control: false,
        })
        .unwrap();
    receive(client, |message| match message {
        ServerMessage::Reply(reply) => Some(reply),
        _ => None,
    })
}

#[test]
fn native_reported_colors_are_query_only_and_survive_reconnect() {
    let host = host();
    let admin = host.connect().unwrap();
    assert_eq!(
        command(
            &admin,
            &["new-session", "-d", "-s", "colors", "sleep", "30"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(
            &admin,
            &["set-option", "-g", "window-style", "fg=#112233,bg=#040506"],
            None,
            false
        )
        .error,
        None
    );
    let view = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    let snapshot = |client: &LocalClient| {
        receive(client, |message| match message {
            ServerMessage::Snapshot { state, .. } => Some(state),
            _ => None,
        })
    };
    assert_eq!(
        command(&view, &["attach-session", "-t", "colors"], None, true).error,
        None
    );
    let initial = snapshot(&view);
    let pid = initial["panes"][0]["pid"].clone();
    for value in [
        "%0:\x1b]10;rgb:8000/ab00/ffff\x1b\\",
        "%0:\x1b]11;#ffffff\x07",
    ] {
        assert_eq!(
            command(&view, &["refresh-client", "-r", value], None, true).error,
            None
        );
        snapshot(&view);
    }
    assert_eq!(
        command(&view, &["display-message", "-p", "ready"], None, true).error,
        None
    );
    let state = snapshot(&view);
    let terminal: tmuxx_terminal::Snapshot =
        serde_json::from_value(state["panes"][0]["terminal"].clone()).unwrap();
    assert_eq!(terminal.foreground, [0x11, 0x22, 0x33]);
    assert_eq!(terminal.background, [4, 5, 6]);
    let mut restored = terminal.restore().unwrap();
    let replies = restored
        .feed(b"\x1b]10;?\x07\x1b]11;?\x07\x1b[?996n")
        .into_iter()
        .filter_map(|effect| match effect {
            tmuxx_terminal::Effect::Write(bytes) => Some(bytes),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        replies,
        b"\x1b]10;rgb:8080/abab/ffff\x07\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?997;2n"
    );
    restored.feed(b"\x1bc");
    assert_eq!(
        restored.control_colors(),
        [Some([128, 171, 255]), Some([255; 3])]
    );
    drop(view);
    let second = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        command(&second, &["attach-session", "-t", "colors"], None, true).error,
        None
    );
    let reconnected = snapshot(&second);
    assert_eq!(reconnected["panes"][0]["pid"], pid);
    let terminal: tmuxx_terminal::Snapshot =
        serde_json::from_value(reconnected["panes"][0]["terminal"].clone()).unwrap();
    assert_eq!(
        terminal.settings.control_colors,
        [Some([128, 171, 255]), Some([255; 3])]
    );
    host.shutdown().unwrap();
}

#[test]
fn one_view_handles_clipboard_and_reply_cannot_be_replayed() {
    let host = host();
    let admin = host.connect().unwrap();
    assert_eq!(
        command(
            &admin,
            &["set-option", "-s", "set-clipboard", "on"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(
            &admin,
            &["set-option", "-s", "get-clipboard", "request"],
            None,
            false
        )
        .error,
        None
    );
    let script = "import os,tty; tty.setraw(0); os.write(1,b'READY'); os.read(0,1); os.write(1,b'\x1b]52;c;?\x07'); data=b''\nwhile not data.endswith(b'\x07'): data+=os.read(0,1)\nos.write(1,b'REPLY:'+data.hex().encode()); os.read(0,1)";
    assert_eq!(
        command(
            &admin,
            &[
                "new-session",
                "-d",
                "-s",
                "shared",
                "python3",
                "-u",
                "-c",
                script
            ],
            None,
            false
        )
        .error,
        None
    );
    let unrelated = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        command(
            &unrelated,
            &["new-session", "-s", "unrelated", "sleep", "30"],
            None,
            true
        )
        .error,
        None
    );
    receive(&unrelated, |message| {
        matches!(message, ServerMessage::Snapshot { .. }).then_some(())
    });
    let first = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..ClientContext::default()
        })
        .unwrap();
    let second = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..ClientContext::default()
        })
        .unwrap();
    for view in [&first, &second] {
        assert_eq!(
            command(view, &["attach-session", "-t", "shared"], None, true).error,
            None
        );
        receive(view, |message| {
            matches!(message, ServerMessage::Snapshot { .. }).then_some(())
        });
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !command(&admin, &["capture-pane", "-p"], None, false)
        .output
        .starts_with(b"READY")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        command(&admin, &["send-keys", "-l", "q"], None, false).error,
        None
    );
    let event = receive(&first, |message| match message {
        ServerMessage::HostEffect {
            event_id, action, ..
        } => {
            assert!(action.get("ClipboardRead").is_some());
            Some(event_id)
        }
        _ => None,
    });
    assert!(
        !std::iter::from_fn(|| second.try_recv().ok())
            .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
    );
    assert!(
        !std::iter::from_fn(|| unrelated.try_recv().ok())
            .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
    );
    let event = event.to_string();
    let response = Some(br#"{"Read":{"Success":{"contents":[{"mime":[116,101,120,116,47,112,108,97,105,110],"data":[111,107]}],"available":[],"remember":false}}}"#.to_vec());
    assert!(
        command(
            &second,
            &["tmuxx-host-reply", &event],
            response.clone(),
            false
        )
        .error
        .is_some()
    );
    assert_eq!(
        command(
            &first,
            &["tmuxx-host-reply", &event],
            response.clone(),
            false
        )
        .error,
        None
    );
    assert!(
        command(&first, &["tmuxx-host-reply", &event], response, false)
            .error
            .is_some()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let capture = command(&admin, &["capture-pane", "-p"], None, false).output;
        if String::from_utf8_lossy(&capture).contains("REPLY:1b5d35323b633b6232733d07") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "application did not receive exactly the successful OSC52 response: {capture:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    host.shutdown().unwrap();
}

#[test]
fn clipboard_defaults_block_writes_and_opt_in_fires_buffer_notification_and_hook() {
    let host = host();
    let admin = host.connect().unwrap();
    let script = "import os,tty\ntty.setraw(0)\nos.write(1,b'READY')\nos.read(0,1)\nos.write(1,b'\\x1b]52;c;bm8=\\x07BLOCKED')\nos.read(0,1)\nos.write(1,b'\\x1b]52;cp;b2s=\\x07ALLOWED')\nos.read(0,1)";
    assert_eq!(
        command(
            &admin,
            &[
                "new-session",
                "-d",
                "-s",
                "clipboard",
                "python3",
                "-u",
                "-c",
                script
            ],
            None,
            false
        )
        .error,
        None
    );
    let view = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        command(&view, &["attach-session", "-t", "clipboard"], None, true).error,
        None
    );
    receive(&view, |message| {
        matches!(message, ServerMessage::Snapshot { .. }).then_some(())
    });
    let wait = |needle: &[u8]| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let capture = command(&admin, &["capture-pane", "-p"], None, false).output;
            if capture.windows(needle.len()).any(|word| word == needle) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "missing marker {needle:?}: {capture:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait(b"READY");
    assert_eq!(
        command(&admin, &["send-keys", "-l", "q"], None, false).error,
        None
    );
    wait(b"BLOCKED");
    assert!(
        command(
            &admin,
            &["list-buffers", "-F", "#{buffer_name}"],
            None,
            false
        )
        .output
        .is_empty()
    );
    assert!(
        !std::iter::from_fn(|| view.try_recv().ok())
            .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
    );
    assert_eq!(
        command(
            &admin,
            &["set-option", "-s", "set-clipboard", "on"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(
            &admin,
            &[
                "set-hook",
                "-p",
                "-t",
                "%0",
                "pane-set-clipboard",
                "set-option -Fg @clipboard-hook '#{hook_pane}'"
            ],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(&admin, &["send-keys", "-l", "q"], None, false).error,
        None
    );
    let mut notifications = 0;
    receive(&view, |message| match message {
        ServerMessage::Control { data } if data == b"%paste-buffer-changed buffer0" => {
            notifications += 1;
            None
        }
        ServerMessage::HostEffect { action, .. } => {
            assert!(action.get("ClipboardWrite").is_some());
            Some(())
        }
        _ => None,
    });
    assert_eq!(notifications, 1);
    wait(b"ALLOWED");
    assert_eq!(command(&admin, &["show-buffer"], None, false).output, b"ok");
    let deadline = Instant::now() + Duration::from_secs(5);
    while command(
        &admin,
        &["show-options", "-gv", "@clipboard-hook"],
        None,
        false,
    )
    .output
        != b"%0\n"
    {
        assert!(
            Instant::now() < deadline,
            "pane-set-clipboard hook did not run"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    host.shutdown().unwrap();
}

#[test]
fn two_view_sizes_are_arbitrated_without_changing_process_identity() {
    let host = host();
    let first = host.connect().unwrap();
    let second = host.connect().unwrap();
    assert_eq!(
        command(
            &first,
            &["new-session", "-d", "-s", "shared", "sleep", "30"],
            None,
            true
        )
        .error,
        None
    );
    assert_eq!(
        command(&second, &["attach-session", "-t", "shared"], None, true).error,
        None
    );
    let pid = command(
        &first,
        &["display-message", "-p", "#{pane_pid}"],
        None,
        false,
    )
    .output;
    assert_eq!(
        command(
            &first,
            &["set-window-option", "window-size", "smallest"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(&first, &["refresh-client", "-C", "100,40"], None, false).error,
        None
    );
    assert_eq!(
        command(&second, &["refresh-client", "-C", "60,20"], None, false).error,
        None
    );
    assert_eq!(
        command(
            &first,
            &["display-message", "-p", "#{pane_width}x#{pane_height}"],
            None,
            false
        )
        .output,
        b"60x20\n"
    );
    assert_eq!(
        command(
            &first,
            &["display-message", "-p", "#{pane_pid}"],
            None,
            false
        )
        .output,
        pid
    );
    drop(second);
    let deadline = Instant::now() + Duration::from_secs(5);
    while command(
        &first,
        &["display-message", "-p", "#{pane_width}x#{pane_height}"],
        None,
        false,
    )
    .output
        != b"100x40\n"
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    host.shutdown().unwrap();
}

#[test]
fn retention_acknowledgement_preserves_only_selected_panes_after_all_views_leave() {
    let host = host();
    let admin = host.connect().unwrap();
    let first = host.connect().unwrap();
    let second = host.connect().unwrap();
    assert_eq!(
        command(
            &first,
            &["new-session", "-d", "-s", "retention", "sleep", "30"],
            None,
            true
        )
        .error,
        None
    );
    assert_eq!(
        command(
            &first,
            &["split-window", "-h", "-t", "%0", "sleep", "30"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(
            &first,
            &["tmuxx-retain-pane", "-t", "%0", "off"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(
            &first,
            &["tmuxx-retain-pane", "-t", "%1", "on"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(&second, &["tmuxx-claim-pane", "-t", "%0"], None, false).error,
        None
    );
    let pid = command(
        &admin,
        &["display-message", "-p", "-t", "%1", "#{pane_pid}"],
        None,
        false,
    )
    .output;
    drop(first);
    assert_eq!(
        command(
            &admin,
            &["list-panes", "-a", "-F", "#{pane_id}"],
            None,
            false
        )
        .output,
        b"%0\n%1\n"
    );
    drop(second);
    let deadline = Instant::now() + Duration::from_secs(5);
    while command(
        &admin,
        &["list-panes", "-a", "-F", "#{pane_id}"],
        None,
        false,
    )
    .output
        != b"%1\n"
    {
        assert!(
            Instant::now() < deadline,
            "ephemeral pane outlived its last view"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let relaunched = host.connect().unwrap();
    assert_eq!(
        command(
            &relaunched,
            &["attach-session", "-t", "retention"],
            None,
            true
        )
        .error,
        None
    );
    assert_eq!(
        command(
            &relaunched,
            &["display-message", "-p", "-t", "%1", "#{pane_pid}"],
            None,
            false
        )
        .output,
        pid
    );
    host.shutdown().unwrap();
}

#[test]
fn native_cell_geometry_updates_terminal_queries_pty_and_pairwise_arbitration() {
    let host = host();
    let admin = host.connect().unwrap();
    #[cfg(unix)]
    let script = "import os,tty,fcntl,termios,struct\ntty.setraw(0)\ninitial=struct.unpack('HHHH',fcntl.ioctl(0,termios.TIOCGWINSZ,b'\\0'*8))\nos.write(1,('INITIAL='+','.join(map(str,initial))+'\\r\\nREADY').encode())\nwhile os.read(0,1):\n size=struct.unpack('HHHH',fcntl.ioctl(0,termios.TIOCGWINSZ,b'\\0'*8))\n os.write(1,b'\\x1b[16t')\n reply=b''\n while not reply.endswith(b't'): reply+=os.read(0,1)\n os.write(1,(' SIZE='+','.join(map(str,size))+' REPLY='+reply.hex()+'\\r\\n').encode())";
    // ConPTY exposes cell dimensions through the console API, not TIOCGWINSZ
    // pixels. Read those real cells and query the terminal's actual pixel size.
    #[cfg(windows)]
    let windows_script = format!(
        "{RAW_CONSOLE}{}",
        r#"class Coord(ctypes.Structure):
 _fields_=[('x',ctypes.c_short),('y',ctypes.c_short)]
class Rect(ctypes.Structure):
 _fields_=[('left',ctypes.c_short),('top',ctypes.c_short),('right',ctypes.c_short),('bottom',ctypes.c_short)]
class Info(ctypes.Structure):
 _fields_=[('size',Coord),('cursor',Coord),('attributes',ctypes.c_ushort),('window',Rect),('maximum',Coord)]
k.GetConsoleScreenBufferInfo.argtypes=[ctypes.c_void_p,ctypes.POINTER(Info)]
def query(code):
 os.write(1,code)
 reply=b''
 while not reply.endswith(b't'): reply+=os.read(0,1)
 return reply
def size():
 info=Info()
 assert k.GetConsoleScreenBufferInfo(k.GetStdHandle(-11),ctypes.byref(info))
 pixels=query(b'\x1b[14t')
 assert pixels.startswith(b'\x1b[4;'),repr(pixels)
 height,width=map(int,pixels[4:-1].split(b';'))
 return info.window.bottom-info.window.top+1,info.window.right-info.window.left+1,width,height
os.write(1,('INITIAL='+','.join(map(str,size()))+'\r\nREADY').encode())
while os.read(0,1):
 current=size()
 reply=query(b'\x1b[16t')
 os.write(1,(' SIZE='+','.join(map(str,current))+' REPLY='+reply.hex()+'\r\n').encode())
"#
    );
    #[cfg(windows)]
    let script = windows_script.as_str();
    assert_eq!(
        command(
            &admin,
            &[
                "new-session",
                "-d",
                "-s",
                "pixels",
                "python3",
                "-u",
                "-c",
                script
            ],
            None,
            false
        )
        .error,
        None
    );
    let wait = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = command(&admin, &["capture-pane", "-p"], None, false).output;
            if String::from_utf8_lossy(&output).contains(needle) {
                break;
            }
            assert!(Instant::now() < deadline, "missing {needle}: {output:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait("READY");
    wait("INITIAL=24,80,1280,768");
    let view = host.connect().unwrap();
    for args in [
        vec!["attach-session", "-t", "pixels"],
        vec!["refresh-client", "-C", "80,24"],
        vec!["tmuxx-cell-size", "10", "20"],
    ] {
        assert_eq!(command(&view, &args, None, true).error, None);
    }
    assert_eq!(
        command(&admin, &["send-keys", "-l", "x"], None, false).error,
        None
    );
    wait("SIZE=24,80,800,480 REPLY=1b5b363b32303b313074");
    assert!(
        command(&view, &["tmuxx-cell-size", "0", "20"], None, false)
            .error
            .is_some()
    );
    let second = host.connect().unwrap();
    for args in [
        vec!["attach-session", "-t", "pixels"],
        vec!["refresh-client", "-C", "60,20"],
        vec!["tmuxx-cell-size", "14", "18"],
    ] {
        assert_eq!(command(&second, &args, None, true).error, None);
    }
    assert_eq!(
        command(&admin, &["send-keys", "-l", "x"], None, false).error,
        None
    );
    wait("SIZE=20,60,840,360 REPLY=1b5b363b31383b313474");
    assert_eq!(
        command(
            &admin,
            &["set-option", "-w", "window-size", "smallest"],
            None,
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(&admin, &["send-keys", "-l", "x"], None, false).error,
        None
    );
    wait("SIZE=20,60,600,400 REPLY=1b5b363b32303b313074");
    let result = command(
        &view,
        &[
            "display-message",
            "-p",
            "#{client_cell_width},#{client_cell_height}:#{window_cell_width},#{window_cell_height}",
        ],
        None,
        false,
    );
    assert_eq!(result.output, b"10,20:10,20\n");
    let result = command(
        &view,
        &[
            "display-message",
            "-p",
            "#{client_width},#{client_height}:#{client_control_mode}",
        ],
        None,
        false,
    );
    assert_eq!(result.output, b"80,24:0\n");
    for command_name in ["respawn-pane", "respawn-window"] {
        assert_eq!(
            command(&admin, &[command_name, "-k"], None, false).error,
            None
        );
        wait("INITIAL=20,60,600,400");
        assert_eq!(
            command(&admin, &["send-keys", "-l", "x"], None, false).error,
            None
        );
        wait("SIZE=20,60,600,400 REPLY=1b5b363b32303b313074");
    }
    for (client, name, initial) in [
        (&admin, "new-window", "20,60,600,400"),
        (&view, "new-session", "24,80,800,480"),
    ] {
        let reply = command(
            client,
            &[
                name,
                "-dP",
                "-F",
                "#{pane_id}",
                "python3",
                "-u",
                "-c",
                script,
            ],
            None,
            false,
        );
        assert_eq!(reply.error, None);
        let target = std::str::from_utf8(&reply.output).unwrap().trim();
        let expected = format!("INITIAL={initial}");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = command(&admin, &["capture-pane", "-p", "-t", target], None, false).output;
            if String::from_utf8_lossy(&output).contains(&expected) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{name}: expected {expected}, got {output:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    host.shutdown().unwrap();
}

#[test]
fn native_style_theme_and_active_pane_survive_view_reconnect() {
    let host = host();
    let admin = host.connect().unwrap();
    for args in [
        vec!["new-session", "-d", "-s", "styles", "sleep", "30"],
        vec!["split-window", "-d", "sleep", "30"],
        vec!["set", "-g", "window-style", "bg=themeblack"],
        vec!["set", "-g", "window-active-style", "bg=#123456"],
    ] {
        assert_eq!(command(&admin, &args, None, false).error, None);
    }
    let snapshot = |client: &LocalClient| {
        receive(client, |message| match message {
            ServerMessage::Snapshot { state, .. } => Some(state),
            _ => None,
        })
    };
    let pane = |state: &serde_json::Value, id: u64| -> tmuxx_terminal::Snapshot {
        serde_json::from_value(
            state["panes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == id)
                .unwrap()["terminal"]
                .clone(),
        )
        .unwrap()
    };
    let first = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        command(&first, &["attach", "-t", "styles"], None, true).error,
        None
    );
    let initial = snapshot(&first);
    let pid = initial["panes"][0]["pid"].clone();
    assert_eq!(pane(&initial, 0).background, [0x12, 0x34, 0x56]);
    assert_eq!(
        command(
            &first,
            &["tmuxx-host-state", "-t", "%1"],
            Some(br#"{"focused":false,"visible":true,"color_scheme":"light"}"#.to_vec()),
            true
        )
        .error,
        None
    );
    let light = snapshot(&first);
    assert_eq!(pane(&light, 1).background, [242; 3]);
    assert_eq!(pane(&light, 1).settings.host_dark, Some(false));
    assert_eq!(
        command(
            &first,
            &["display-message", "-p", "#{client_theme}"],
            None,
            true
        )
        .output,
        b"light\n"
    );
    assert_eq!(
        command(&first, &["select-pane", "-t", "%1"], None, true).error,
        None
    );
    let selected = snapshot(&first);
    assert_eq!(pane(&selected, 1).background, [0x12, 0x34, 0x56]);
    assert_eq!(pane(&selected, 0).background, [13; 3]);
    drop(first);
    let second = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        command(&second, &["attach", "-t", "styles"], None, true).error,
        None
    );
    let restored = snapshot(&second);
    assert_eq!(restored["panes"][0]["pid"], pid);
    let terminal = pane(&restored, 1).restore().unwrap().snapshot();
    assert_eq!(terminal.background, [0x12, 0x34, 0x56]);
    assert_eq!(terminal.settings.host_dark, Some(false));
    host.shutdown().unwrap();
}

#[test]
fn buffer_clipboard_flags_target_exact_native_view_and_keep_binary_data() {
    let host = host();
    let admin = host.connect().unwrap();
    assert_eq!(
        command(
            &admin,
            &["new", "-d", "-s", "clipboard", "sleep", "30"],
            None,
            false
        )
        .error,
        None
    );
    let first = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    let second = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    for view in [&first, &second] {
        assert_eq!(
            command(view, &["attach", "-t", "clipboard"], None, true).error,
            None
        );
    }
    let first_name = String::from_utf8(
        command(&first, &["display", "-p", "#{client_name}"], None, false).output,
    )
    .unwrap()
    .trim()
    .to_owned();
    let second_name = String::from_utf8(
        command(&second, &["display", "-p", "#{client_name}"], None, false).output,
    )
    .unwrap()
    .trim()
    .to_owned();
    for (target, other, name, data, load) in [
        (
            &second,
            &first,
            second_name.as_str(),
            b"plain".as_slice(),
            false,
        ),
        (
            &first,
            &second,
            first_name.as_str(),
            b"a\0b\n\xff".as_slice(),
            true,
        ),
    ] {
        let response = if load {
            command(
                &admin,
                &["load-buffer", "-w", "-t", name, "-b", "retained", "-"],
                Some(data.to_vec()),
                false,
            )
        } else {
            command(
                &admin,
                &["set-buffer", "-w", "-t", name, "-b", "retained", "plain"],
                None,
                false,
            )
        };
        assert_eq!(response.error, None);
        receive(target, |message| match message {
            ServerMessage::HostEffect {
                pane_id, action, ..
            } => {
                assert_eq!(pane_id, 0);
                let tmuxx_core::HostAction::ClipboardWrite(request) =
                    serde_json::from_value(action).unwrap()
                else {
                    panic!("expected clipboard write")
                };
                assert_eq!(request.contents[0].data.as_ref(), data);
                Some(())
            }
            _ => None,
        });
        assert_eq!(
            command(&admin, &["show-buffer", "-b", "retained"], None, false).output,
            data
        );
        while let Ok(message) = other.try_recv() {
            assert!(!matches!(message, ServerMessage::HostEffect { .. }));
        }
    }
    assert_eq!(
        command(
            &admin,
            &["load-buffer", "-b", "retained", "-"],
            Some(Vec::new()),
            false
        )
        .error,
        None
    );
    assert_eq!(
        command(&admin, &["show-buffer", "-b", "retained"], None, false).output,
        b"a\0b\n\xff"
    );
    for invalid in ["bad\r", "bad\x1b\n%exit", "bad\x7f"] {
        for load in [false, true] {
            let response = if load {
                command(
                    &admin,
                    &["load-buffer", "-w", "-t", &first_name, "-b", invalid, "-"],
                    Some(b"rejected".to_vec()),
                    false,
                )
            } else {
                command(
                    &admin,
                    &[
                        "set-buffer",
                        "-w",
                        "-t",
                        &first_name,
                        "-b",
                        invalid,
                        "rejected",
                    ],
                    None,
                    false,
                )
            };
            assert_eq!(
                response.error,
                Some(format!(
                    "invalid buffer name: {}",
                    invalid.as_bytes().escape_ascii()
                ))
            );
            assert_eq!(
                command(&admin, &["show-buffer", "-b", "retained"], None, false).output,
                b"a\0b\n\xff"
            );
            for view in [&first, &second] {
                // This reply is a queue barrier for every preceding host event.
                view.send(Request {
                    request_id: 99,
                    argv: vec![
                        b"display-message".to_vec(),
                        b"-p".to_vec(),
                        b"checked".to_vec(),
                    ],
                    stdin: None,
                    subscribe: true,
                    control: false,
                })
                .unwrap();
                receive(view, |message| {
                    assert!(!matches!(message, ServerMessage::HostEffect { .. }));
                    if let ServerMessage::Reply(reply) = message {
                        assert_eq!(reply.output, b"checked\n");
                        Some(())
                    } else {
                        None
                    }
                });
            }
        }
        assert_eq!(
            command(
                &admin,
                &["load-buffer", "-w", "-t", &first_name, "-b", invalid, "-"],
                Some(Vec::new()),
                false
            )
            .error,
            None
        );
    }
    host.shutdown().unwrap();
}

#[test]
fn agent_status_is_retained_without_views_and_reconnect_keeps_the_process() {
    let host = host();
    let admin = host.connect().unwrap();
    let frame = "\x1b]777;rustty-agent;1;eyJvcCI6ImJlZ2luIiwic3RhdGUiOiJuZWVkc19pbnB1dCIsInRocmVhZF9pZCI6InJlbW90ZSJ9\x07";
    #[cfg(unix)]
    let program = [
        "/bin/sh",
        "-c",
        "printf '%s' \"$1\"; printf READY; sleep 30",
        "sh",
        frame,
    ];
    #[cfg(windows)]
    let script = format!(
        "{RAW_CONSOLE}\nimport sys,time\nos.write(1,sys.argv[1].encode('utf-8')+b'READY')\ntime.sleep(30)"
    );
    #[cfg(windows)]
    let program = ["python", "-u", "-c", script.as_str(), frame];
    let mut argv = vec!["new-session", "-d", "-s", "agent"];
    argv.extend(program);
    assert_eq!(command(&admin, &argv, None, false).error, None);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !command(&admin, &["capture-pane", "-p", "-t", "%0"], None, false)
        .output
        .starts_with(b"READY")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut pid = None;
    for _ in 0..2 {
        let view = host.connect().unwrap();
        assert_eq!(
            command(&view, &["attach-session", "-t", "agent"], None, true).error,
            None
        );
        let state = receive(&view, |message| match message {
            ServerMessage::Snapshot { state, .. } => Some(state),
            ServerMessage::HostEffect { .. } => panic!("attachment replayed a host action"),
            _ => None,
        });
        let terminal: tmuxx_terminal::Snapshot =
            serde_json::from_value(state["panes"][0]["terminal"].clone()).unwrap();
        assert_eq!(
            terminal
                .agent_status
                .snapshot
                .as_ref()
                .unwrap()
                .thread_id
                .as_deref(),
            Some("remote")
        );
        assert_eq!(terminal.agent_status.generation, 1);
        if let Some(previous) = &pid {
            assert_eq!(previous, &state["panes"][0]["pid"]);
        }
        pid = Some(state["panes"][0]["pid"].clone());
    }
    host.shutdown().unwrap();
}
