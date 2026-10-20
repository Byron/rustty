#![cfg(windows)]

use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Hello, Request, ServerMessage};
use windows_sys::Win32::{Foundation::*, System::Threading::*};

const EXE: &str = env!("CARGO_BIN_EXE_tmuxx");
static NEXT: AtomicU64 = AtomicU64::new(0);

fn python() -> &'static str {
    static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PYTHON.get_or_init(|| {
        let result = bounded(Command::new("python").args([
            "-c",
            "import sys; sys.stdout.buffer.write(sys.executable.encode('utf-8'))",
        ]));
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap()
    })
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    owner: OwnedChild,
    label: String,
    path: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.owner.0.kill();
        let _ = self.owner.0.wait();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn bounded(command: &mut Command) -> Output {
    bounded_input(command, None)
}

fn bounded_input(command: &mut Command, input: Option<&[u8]>) -> Output {
    let mut child = OwnedChild(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).unwrap();
        b
    });
    let err = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).unwrap();
        b
    });
    if let Some(input) = input {
        child.0.stdin.take().unwrap().write_all(input).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "bounded CLI command timed out");
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

impl Fixture {
    fn new() -> Self {
        let label = format!(
            "fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(&label);
        std::fs::create_dir(&path).unwrap();
        let owner = OwnedChild(
            Command::new(EXE)
                .args(["-D", "-L", &label])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let mut fixture = Self { owner, label, path };
        let endpoint = tmuxx_runtime::windows::endpoint(&fixture.label).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(mut connection) = tmuxx_runtime::windows::connect(&endpoint) {
                let hello = tmuxx_runtime::native_handshake(&mut connection).unwrap();
                assert!(hello.instance_id.is_some());
                break;
            }
            assert!(
                fixture.owner.0.try_wait().unwrap().is_none(),
                "owner startup failed; TMUXX_CONPTY_RESOURCES must name the pinned bundle"
            );
            assert!(Instant::now() < deadline, "owner readiness timeout");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }
    fn command(&self, args: &[&str]) -> Vec<u8> {
        let output = bounded(Command::new(EXE).args(["-L", &self.label]).args(args));
        assert!(
            output.status.success(),
            "command {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn pane(&self) -> (u32, u32) {
        let script = self.path.join("pane 雪 & source.py");
        let marker = self.path.join("pids.json");
        std::fs::write(
            &script,
            r#"import json,os,pathlib,subprocess,sys,time
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(90)'])
pathlib.Path(sys.argv[1]).write_text(json.dumps([os.getpid(),child.pid]))
print('TMUXX_READY_' + str(os.getpid()),flush=True)
for line in sys.stdin:
    print('RECEIVED:' + line.strip(),flush=True)
"#,
        )
        .unwrap();
        self.command(&[
            "new-session",
            "-d",
            "-s",
            "session 雪 &; value",
            python(),
            script.to_str().unwrap(),
            marker.to_str().unwrap(),
        ]);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(bytes) = std::fs::read(&marker)
                && let Ok(pids) = serde_json::from_slice::<(u32, u32)>(&bytes)
            {
                return pids;
            }
            assert!(
                Instant::now() < deadline,
                "pane never wrote process identity"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn alive(pid: u32) -> bool {
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return false;
    }
    let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
    unsafe {
        CloseHandle(process);
    }
    running
}

fn exited(pids: &[u32]) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while pids.iter().copied().any(alive) {
        assert!(
            Instant::now() < deadline,
            "owner leaked descendants {pids:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_cli_and_fixed_stdio_bridge_share_live_panes() {
    let mut fixture = Fixture::new();
    let (pane, descendant) = fixture.pane();
    let expected = format!("{pane}\n");
    assert_eq!(
        fixture.command(&["list-panes", "-a", "-F", "#{pane_pid}"]),
        expected.as_bytes()
    );
    fixture.command(&[
        "link-window",
        "-s",
        "session 雪 &; value:0",
        "-t",
        "session 雪 &; value:4",
    ]);
    let windows = fixture.command(&["list-windows", "-a", "-F", "#{window_id}:#{window_index}"]);
    assert!(String::from_utf8(windows).unwrap().contains("@0:4"));
    let mut bridge = OwnedChild(
        Command::new(EXE)
            .arg("--native-stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut input = bridge.0.stdin.take().unwrap();
    serde_json::to_writer(
        &mut input,
        &Hello {
            instance: Some(fixture.label.clone()),
            ..Hello::default()
        },
    )
    .unwrap();
    input.write_all(b"\n").unwrap();
    let mut output = BufReader::new(bridge.0.stdout.take().unwrap());
    let hello = tmuxx_runtime::read_record(&mut output, 1024)
        .unwrap()
        .unwrap();
    assert!(
        serde_json::from_slice::<Hello>(&hello)
            .unwrap()
            .instance_id
            .is_some()
    );
    let request = Request {
        request_id: 7,
        argv: ["list-panes", "-a", "-F", "#{pane_pid}"]
            .map(|s| s.as_bytes().to_vec())
            .to_vec(),
        stdin: None,
        subscribe: false,
        control: false,
    };
    serde_json::to_writer(&mut input, &request).unwrap();
    input.write_all(b"\n").unwrap();
    let response = tmuxx_runtime::read_record(&mut output, 4096)
        .unwrap()
        .unwrap();
    match serde_json::from_slice::<ServerMessage>(&response).unwrap() {
        ServerMessage::Reply(reply) => {
            assert_eq!(reply.request_id, 7);
            assert_eq!(reply.output, expected.repeat(2).as_bytes());
            assert!(reply.error.is_none());
        }
        other => panic!("unexpected bridge response {other:?}"),
    }
    drop(bridge);
    assert!(fixture.owner.0.try_wait().unwrap().is_none());
    assert!(alive(pane) && alive(descendant));
    fixture.command(&["send-keys", "-t", "%0", "-l", "snow 雪 &;|"]);
    fixture.command(&["send-keys", "-t", "%0", "Enter"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let capture = fixture.command(&["capture-pane", "-p", "-t", "%0"]);
        if String::from_utf8(capture)
            .unwrap()
            .contains("RECEIVED:snow 雪 &;|")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "input did not reach the original process"
        );
    }
    drop(fixture);
    exited(&[pane, descendant]);
}

#[test]
fn graceful_and_abrupt_owner_exit_reclaim_descendants_only() {
    let unrelated = OwnedChild(
        Command::new(python())
            .args([
                "-c",
                "import time; time.sleep(90) # tmuxx-windows-unrelated-fixture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for graceful in [true, false] {
        let mut fixture = Fixture::new();
        let (pane, descendant) = fixture.pane();
        assert!(alive(pane) && alive(descendant));
        if graceful {
            fixture.command(&["kill-server"]);
        } else {
            fixture.owner.0.kill().unwrap();
        }
        exited(&[pane, descendant]);
        assert!(
            alive(unrelated.0.id()),
            "an unrelated process was terminated"
        );
    }
}

#[test]
fn missing_and_modified_packaged_resources_fail_before_owner_start() {
    let directory = std::env::temp_dir().join(format!("tmuxx-bad-package-{}", std::process::id()));
    std::fs::create_dir_all(directory.join("conpty")).unwrap();
    let output = bounded(
        Command::new(EXE)
            .args(["-D", "-L", "bad-package"])
            .env("TMUXX_CONPTY_RESOURCES", &directory),
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("bundled ConPTY")
    );
    std::fs::write(directory.join("conpty/conpty.dll"), b"incompatible").unwrap();
    let output = bounded(
        Command::new(EXE)
            .args(["-D", "-L", "bad-package"])
            .env("TMUXX_CONPTY_RESOURCES", &directory),
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Incompatible bundled ConPTY")
    );
    assert!(
        tmuxx_runtime::windows::connect(&tmuxx_runtime::windows::endpoint("bad-package").unwrap())
            .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

fn pipe(fixture: &Fixture) -> tmuxx_runtime::windows::PipeStream {
    let stream =
        tmuxx_runtime::windows::connect(&tmuxx_runtime::windows::endpoint(&fixture.label).unwrap())
            .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn record(reader: &mut impl std::io::BufRead) -> Option<Vec<u8>> {
    tmuxx_runtime::read_record(reader, 16 * 1024 * 1024).unwrap()
}

fn send(stream: &mut impl Write, value: &impl serde::Serialize) {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    // Every possible record boundary passes through real overlapped pipe I/O.
    for byte in bytes {
        stream.write_all(&[byte]).unwrap();
    }
}

fn request(id: u64, args: &[&str]) -> Request {
    Request {
        request_id: id,
        argv: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
        stdin: None,
        subscribe: false,
        control: false,
    }
}

#[test]
fn malformed_version_and_partial_eof_never_mutate_or_replace_owner() {
    let fixture = Fixture::new();
    fixture.pane();
    let initial = fixture.command(&["list-panes", "-a", "-F", "#{pane_pid}"]);
    let mut stream = pipe(&fixture);
    send(
        &mut stream,
        &Hello {
            version: u32::MAX,
            ..Hello::default()
        },
    );
    let mut reader = BufReader::new(stream);
    let hello: Hello = serde_json::from_slice(&record(&mut reader).unwrap()).unwrap();
    assert_eq!(hello.version, tmuxx_protocol::native::VERSION);
    assert!(record(&mut reader).is_none());
    for malformed in [b"not json\n".as_slice(), b"{\"request_id\":1,\"argv\":"] {
        let mut stream = pipe(&fixture);
        tmuxx_runtime::native_handshake(&mut stream).unwrap();
        stream.write_all(malformed).unwrap();
        if !malformed.ends_with(b"\n") {
            // Abrupt EOF midway through a request must not execute its prefix.
            drop(stream);
        } else {
            assert!(record(&mut BufReader::new(stream)).is_none());
        }
    }
    let mut stream = pipe(&fixture);
    tmuxx_runtime::native_handshake(&mut stream).unwrap();
    send(
        &mut stream,
        &request(99, &["set-buffer", "-b", "binary", "snow 雪"]),
    );
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut reader = BufReader::new(stream);
    let message: ServerMessage = serde_json::from_slice(&record(&mut reader).unwrap()).unwrap();
    assert!(
        matches!(message, ServerMessage::Reply(reply) if reply.request_id == 99 && reply.error.is_none())
    );
    assert!(record(&mut reader).is_none());
    assert_eq!(
        fixture.command(&["show-buffer", "-b", "binary"]),
        "snow 雪".as_bytes()
    );
    assert_eq!(
        fixture.command(&["list-panes", "-a", "-F", "#{pane_pid}"]),
        initial
    );
}

#[test]
fn replacement_incarnation_rejects_stale_mutations_and_discovery_is_live() {
    let mut fixture = Fixture::new();
    let endpoint = tmuxx_runtime::windows::endpoint(&fixture.label).unwrap();
    let original = tmuxx_runtime::native_handshake(&mut pipe(&fixture)).unwrap();
    assert!(
        tmuxx_runtime::windows::discover()
            .unwrap()
            .contains(&endpoint)
    );
    let competing = bounded(Command::new(EXE).args(["-D", "-L", &fixture.label]));
    assert!(!competing.status.success(), "a live owner was replaced");
    fixture.owner.0.kill().unwrap();
    fixture.owner.0.wait().unwrap();
    assert!(
        !tmuxx_runtime::windows::discover()
            .unwrap()
            .contains(&endpoint)
    );
    assert!(tmuxx_runtime::windows::connect(&endpoint).is_err());
    fixture.owner = OwnedChild(
        Command::new(EXE)
            .args(["-D", "-L", &fixture.label])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let replacement = loop {
        if let Ok(mut stream) = tmuxx_runtime::windows::connect(&endpoint) {
            break tmuxx_runtime::native_handshake(&mut stream).unwrap();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_ne!(original.instance_id, replacement.instance_id);
    let mut stream = pipe(&fixture);
    // Pipeline a mutation behind stale identity, including when all bytes arrive
    // in the same ReadFile operation. Nothing after the hello may be submitted.
    let mut bytes = serde_json::to_vec(&Hello {
        instance_id: original.instance_id,
        ..Hello::default()
    })
    .unwrap();
    bytes.push(b'\n');
    bytes.extend(
        serde_json::to_vec(&request(
            4,
            &["set-buffer", "-b", "must-not-exist", "mutation"],
        ))
        .unwrap(),
    );
    bytes.push(b'\n');
    stream.write_all(&bytes).unwrap();
    let mut reader = BufReader::new(stream);
    let observed: Hello = serde_json::from_slice(&record(&mut reader).unwrap()).unwrap();
    assert_eq!(observed.instance_id, replacement.instance_id);
    assert!(record(&mut reader).is_none());
    assert!(fixture.command(&["list-buffers"]).is_empty());
    fixture.command(&["set-buffer", "-b", "committed", "one mutation"]);
    for _ in 0..3 {
        let mut stream = pipe(&fixture);
        assert_eq!(
            tmuxx_runtime::native_handshake(&mut stream)
                .unwrap()
                .instance_id,
            replacement.instance_id
        );
    }
    assert_eq!(
        fixture.command(&["list-buffers", "-F", "#{buffer_name}"]),
        b"committed\n"
    );
}

#[test]
fn snapshot_stream_reconnect_preserves_shared_graph_and_live_processes() {
    let fixture = Fixture::new();
    let (pid, _) = fixture.pane();
    fixture.command(&[
        "link-window",
        "-s",
        "session 雪 &; value:0",
        "-t",
        "session 雪 &; value:4",
    ]);
    let mut incarnations = Vec::new();
    for _ in 0..3 {
        let mut stream = pipe(&fixture);
        incarnations.push(
            tmuxx_runtime::native_handshake(&mut stream)
                .unwrap()
                .instance_id,
        );
        let mut subscribe = request(8, &["list-panes", "-a", "-F", "#{pane_pid}"]);
        subscribe.subscribe = true;
        send(&mut stream, &subscribe);
        let mut reader = BufReader::with_capacity(1, stream);
        let mut assembler = tmuxx_protocol::native::SnapshotAssembler::default();
        let (mut reply, mut snapshot) = (false, false);
        while !reply || !snapshot {
            let message = serde_json::from_slice(&record(&mut reader).unwrap()).unwrap();
            match assembler.push(message).unwrap() {
                Some(ServerMessage::Reply(result)) => {
                    assert!(result.error.is_none());
                    assert_eq!(result.output, format!("{pid}\n{pid}\n").as_bytes());
                    reply = true;
                }
                Some(ServerMessage::Snapshot {
                    instance_id, state, ..
                }) => {
                    assert_eq!(Some(instance_id), *incarnations.last().unwrap());
                    assert_eq!(state["panes"].as_array().unwrap().len(), 1);
                    assert_eq!(state["panes"][0]["pid"], pid);
                    assert_eq!(state["windows"].as_array().unwrap().len(), 1);
                    assert_eq!(
                        state["sessions"][0]["windows"].as_object().unwrap().len(),
                        2
                    );
                    snapshot = true;
                }
                _ => {}
            }
        }
    }
    assert!(incarnations.iter().all(|id| *id == incarnations[0]));
    assert!(alive(pid));
}

#[test]
fn native_control_and_binary_stdin_preserve_framing_and_shared_owner() {
    let fixture = Fixture::new();
    let (pid, descendant) = fixture.pane();
    let bytes: Vec<u8> = (0..=255).collect();
    let loaded = bounded_input(
        Command::new(EXE).args(["-L", &fixture.label, "load-buffer", "-b", "binary", "-"]),
        Some(&bytes),
    );
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(loaded.stdout.is_empty());
    assert_eq!(
        fixture.command(&["save-buffer", "-b", "binary", "-"]),
        bytes
    );
    let control = bounded_input(
        Command::new(EXE).args([
            "-L",
            &fixture.label,
            "-C",
            "attach-session",
            "-t",
            "session 雪 &; value",
        ]),
        Some(
            "display-message -p 'snow 雪 & |; value'\ndisplay-message -p '#{pane_pid}'\n"
                .as_bytes(),
        ),
    );
    assert!(
        control.status.success(),
        "{}",
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(control.stderr.is_empty());
    let mut parser = tmuxx_protocol::Decoder::default();
    // Parse actual CLI stdout through the shared C control codec, one byte at
    // a time. JSON, diagnostics, CRLF conversion or partial guards cannot pass.
    let mut events = Vec::new();
    for byte in &control.stdout {
        events.extend(parser.feed(&[*byte]).unwrap());
    }
    parser.finish().unwrap();
    assert!(!events.is_empty());
    let text = String::from_utf8(control.stdout).unwrap();
    assert!(text.contains("\nsnow 雪 & |; value\n"), "{text}");
    assert!(text.contains(&format!("\n{pid}\n")), "{text}");
    assert!(alive(pid) && alive(descendant));
}
