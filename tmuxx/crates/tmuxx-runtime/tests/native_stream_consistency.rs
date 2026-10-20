#![cfg(any(unix, windows))]
#[cfg(windows)]
#[path = "support/windows_fixture.rs"]
mod windows_fixture;
#[cfg(unix)]
use std::os::unix::net::UnixStream as Stream;
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tmuxx_core::ClientContext;
use tmuxx_protocol::native::{Hello, Request, Response, ServerMessage, SnapshotAssembler};
#[cfg(windows)]
use tmuxx_runtime::windows::PipeStream as Stream;
use tmuxx_runtime::{LocalClient, Options, ServerHandle};

fn host() -> (ServerHandle, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "txstream-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    #[cfg(unix)]
    let path = std::env::temp_dir().join(name);
    #[cfg(windows)]
    let path = tmuxx_runtime::windows::endpoint(&name).unwrap();
    (tmuxx_runtime::start(Options::new(&path)).unwrap(), path)
}
fn receive<T>(client: &LocalClient, mut select: impl FnMut(ServerMessage) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.try_recv() {
            Ok(message) => {
                if let Some(value) = select(message) {
                    return value;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("native viewer disconnected")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1))
            }
        }
        assert!(Instant::now() < deadline, "native receive timed out");
    }
}
fn request(argv: &[&str], stdin: Option<Vec<u8>>, subscribe: bool) -> Request {
    #[cfg(unix)]
    let args = argv.iter().map(|arg| arg.as_bytes().to_vec()).collect();
    #[cfg(windows)]
    let args = windows_fixture::argv(argv);
    Request {
        request_id: 1,
        argv: args,
        stdin,
        subscribe,
        control: false,
    }
}
fn command(
    client: &LocalClient,
    argv: &[&str],
    input: Option<Vec<u8>>,
    subscribe: bool,
) -> Response {
    client.send(request(argv, input, subscribe)).unwrap();
    let response = receive(client, |message| {
        if let ServerMessage::Reply(reply) = message {
            Some(reply)
        } else {
            None
        }
    });
    assert_eq!(response.error, None, "{argv:?}");
    response
}
fn snapshot(client: &LocalClient) -> (u64, serde_json::Value) {
    receive(client, |message| {
        if let ServerMessage::Snapshot {
            sequence, state, ..
        } = message
        {
            Some((sequence, state))
        } else {
            None
        }
    })
}
fn terminal(state: &serde_json::Value) -> tmuxx_terminal::Terminal {
    serde_json::from_value::<tmuxx_terminal::Snapshot>(state["panes"][0]["terminal"].clone())
        .unwrap()
        .restore()
        .unwrap()
}
fn write_record(stream: &mut Stream, value: &impl serde::Serialize) {
    serde_json::to_writer(&mut *stream, value).unwrap();
    stream.write_all(b"\n").unwrap();
}
fn read_record(reader: &mut BufReader<Stream>) -> ServerMessage {
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0, "native EOF");
    serde_json::from_str(&line).unwrap()
}

#[test]
fn live_native_snapshot_has_one_cut_and_updates_follow_its_end() {
    let (host, path) = host();
    let admin = host.connect().unwrap();
    command(
        &admin,
        &["new-session", "-d", "-s", "cut", "-x", "80", "-y", "24", ""],
        None,
        false,
    );
    command(&admin, &["respawn-pane", "-kE"], None, false);
    let pixels = "////".repeat(256 * 256 * 4 / 3) + "/w==";
    command(
        &admin,
        &["display-message", "-I"],
        Some(format!("BEFORE\x1b_Ga=t,i=7,f=32,s=256,v=256;{pixels}\x1b\\").into_bytes()),
        false,
    );
    #[cfg(unix)]
    let mut stream = Stream::connect(path).unwrap();
    #[cfg(windows)]
    let mut stream = tmuxx_runtime::windows::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    write_record(&mut stream, &Hello::default());
    let mut hello = String::new();
    reader.read_line(&mut hello).unwrap();
    assert_eq!(
        serde_json::from_str::<Hello>(&hello).unwrap().version,
        Hello::default().version
    );
    write_record(
        &mut stream,
        &request(&["attach-session", "-t", "cut"], None, true),
    );
    let mut assembler = SnapshotAssembler::default();
    let cut = loop {
        let message = read_record(&mut reader);
        if let ServerMessage::SnapshotStart {
            sequence, bytes, ..
        } = &message
        {
            assert!(*bytes > tmuxx_protocol::native::SNAPSHOT_CHUNK_BYTES as u64);
            let cut = *sequence;
            assert!(assembler.push(message).unwrap().is_none());
            break cut;
        }
    };
    let first_chunk = read_record(&mut reader);
    assert!(matches!(first_chunk, ServerMessage::SnapshotChunk { .. }));
    assert!(assembler.push(first_chunk).unwrap().is_none());

    let after = b"AFTER\x1b[?1049h\x1b[HNEWALT";
    command(
        &admin,
        &["display-message", "-I"],
        Some(after.to_vec()),
        false,
    );
    command(
        &admin,
        &["resize-window", "-x", "60", "-y", "8"],
        None,
        false,
    );
    command(&admin, &["rename-window", "after"], None, false);
    write_record(
        &mut stream,
        &request(&["display-message", "-p", "updated"], None, true),
    );

    let initial = loop {
        let message = read_record(&mut reader);
        let ending = matches!(message, ServerMessage::SnapshotEnd { .. });
        let complete = assembler.push(message).unwrap();
        if let Some(ServerMessage::Snapshot {
            sequence, state, ..
        }) = complete
        {
            assert!(ending);
            assert_eq!(sequence, cut);
            break state;
        }
        assert!(!ending);
    };
    let initial_terminal = terminal(&initial);
    assert_eq!(initial_terminal.size(), (80, 24));
    assert!(initial_terminal.capture().starts_with("BEFORE"));
    assert!(!initial_terminal.snapshot().alternate_active);
    assert_eq!(
        initial_terminal.snapshot().primary_graphics.images[0]
            .pixels
            .len(),
        256 * 256 * 4
    );
    assert_ne!(initial["windows"][0]["name"], "after");

    let mut output = Vec::new();
    let mut update_seen = false;
    let mut latest = cut;
    let final_state = loop {
        if let Some(message) = assembler.push(read_record(&mut reader)).unwrap() {
            match message {
                ServerMessage::PaneOutput {
                    sequence,
                    pane_id,
                    data,
                } => {
                    assert_eq!(pane_id, 0);
                    assert!(sequence > cut && sequence >= latest);
                    latest = sequence;
                    output.extend(data);
                }
                ServerMessage::Changed { sequence } => {
                    assert!(sequence > cut);
                    latest = latest.max(sequence);
                    update_seen = true;
                }
                ServerMessage::Snapshot {
                    sequence, state, ..
                } => {
                    assert!(sequence >= latest && sequence > cut);
                    break state;
                }
                ServerMessage::Reply(reply) => assert_eq!(reply.error, None),
                _ => {}
            }
        }
    };
    assert_eq!(
        output, after,
        "output after the snapshot cut is delivered exactly once"
    );
    assert!(update_seen);
    assert_eq!(final_state["windows"][0]["name"], "after");
    let final_terminal = terminal(&final_state);
    assert_eq!(final_terminal.size(), (60, 8));
    assert!(final_terminal.snapshot().alternate_active);
    assert!(final_terminal.capture().starts_with("NEWALT"));
    assert!(
        final_terminal
            .capture_with(tmuxx_terminal::CaptureOptions {
                alternate: true,
                ..Default::default()
            })
            .unwrap()
            .starts_with(b"BEFOREAFTER")
    );
    assembler.finish().unwrap();
    drop(reader);
    drop(stream);
    host.shutdown().unwrap();
}

#[test]
fn blocked_native_view_disconnects_while_healthy_view_and_pty_survive() {
    let (host, _) = host();
    let admin = host.connect().unwrap();
    let script = "import os,tty\ntty.setraw(0)\nos.write(1,b'READY')\nos.read(0,1)\ndata=b'\\0'*(20*1024*1024)+b'DONE'\nwhile data:data=data[os.write(1,data):]\nos.read(0,1)";
    command(
        &admin,
        &[
            "new-session",
            "-d",
            "-s",
            "backlog",
            "python3",
            "-u",
            "-c",
            script,
        ],
        None,
        false,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !command(&admin, &["capture-pane", "-p"], None, false)
        .output
        .starts_with(b"READY")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let slow = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    let healthy = host
        .connect_with_context(ClientContext {
            host_effects: true,
            ..Default::default()
        })
        .unwrap();
    let mut pid = serde_json::Value::Null;
    for client in [&slow, &healthy] {
        command(client, &["attach-session", "-t", "backlog"], None, true);
        let (_, state) = snapshot(client);
        pid = state["panes"][0]["pid"].clone();
        assert!(pid.as_u64().is_some_and(|pid| pid > 0));
    }
    let healthy_reader = std::thread::spawn(move || {
        let mut bytes = 0;
        let mut zeros = 0;
        let mut tail = Vec::new();
        let mut last = 0;
        receive(&healthy, |message| match message {
            ServerMessage::PaneOutput { sequence, data, .. } => {
                assert!(sequence > last);
                last = sequence;
                bytes += data.len();
                zeros += data.iter().filter(|&&b| b == 0).count();
                tail.extend(data.into_iter().filter(|&b| b != 0));
                tail.ends_with(b"DONE").then_some(())
            }
            _ => None,
        });
        assert_eq!(bytes, 20 * 1024 * 1024 + 4);
        assert_eq!(zeros, 20 * 1024 * 1024);
        assert_eq!(tail, b"DONE");
        healthy
    });
    command(&admin, &["send-keys", "-l", "q"], None, false);
    let healthy = healthy_reader.join().unwrap();
    let mut queued = 0;
    loop {
        match slow.try_recv() {
            Ok(_) => queued += 1,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                panic!("blocked viewer was not disconnected at its bounded backlog")
            }
        }
    }
    assert!(queued <= 256);
    assert!(
        command(&admin, &["capture-pane", "-p"], None, false)
            .output
            .starts_with(b"READYDONE")
    );
    assert_eq!(
        command(
            &admin,
            &["display-message", "-p", "#{pane_pid}:#{pane_dead}"],
            None,
            false
        )
        .output,
        format!("{}:0\n", pid.as_u64().unwrap()).as_bytes()
    );
    let reconnected = host.connect().unwrap();
    command(
        &reconnected,
        &["attach-session", "-t", "backlog"],
        None,
        true,
    );
    let (_, state) = snapshot(&reconnected);
    assert_eq!(state["panes"][0]["pid"], pid);
    assert!(terminal(&state).capture().starts_with("READYDONE"));
    command(&healthy, &["display-message", "-p", "healthy"], None, false);
    host.shutdown().unwrap();
}
