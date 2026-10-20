#![cfg(windows)]

//! Raw byte selectors reach the real Windows stdio bridge as protocol bytes.
//! Native control lines use octal escapes; Windows argv never receives lossy
//! replacement characters for an unrepresentable argument.
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use remote_reference::{RemoteControl, RemoteReference};
use std::os::windows::process::ExitStatusExt;
use std::{
    io::{BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};
use tmuxx_protocol::native::{Hello, Request, ServerMessage};
use tmuxx_protocol::{Decoder, Event};

const EXE: &str = env!("CARGO_BIN_EXE_tmuxx");
const VALID_SESSION: &[u8] = "replace\u{fffd}".as_bytes();
const VALID_WINDOW: &[u8] = "replace\u{fffd}:window\u{fffd}".as_bytes();
const BAD_SESSION: &[u8] = b"replace\xff";
const BAD_SOURCE: &[u8] = b"replace\xff:window\xef\xbf\xbd";
const GRAPH: &[u8] = b"#{session_name}|#{window_id}|#{window_name}|#{pane_id}";

struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

enum Process {
    Native(OwnedChild),
    Reference(RemoteControl),
}
impl Process {
    fn finish(mut self, script: &[u8]) -> Output {
        match &mut self {
            Self::Native(owned) => {
                owned
                    .0
                    .as_mut()
                    .unwrap()
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(script)
                    .unwrap();
                wait_native(owned)
            }
            Self::Reference(child) => {
                assert!(child.write(script).status.success());
                assert!(child.finish_input().status.success());
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(code) = child.exited() {
                        let mut output = child.read();
                        output.status = ExitStatus::from_raw(code as u32);
                        return output;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "reference selector control timed out"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
}

fn wait_native(owned: &mut OwnedChild) -> Output {
    let child = owned.0.as_mut().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "native selector control timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    owned.0.take();
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

struct Fixture {
    dir: PathBuf,
    label: String,
    endpoint: PathBuf,
    reference: RemoteReference,
    host: Option<tmuxx_runtime::ServerHandle>,
}
impl Fixture {
    fn new(label: &str) -> Self {
        let label = format!("selectors-{}-{label}", std::process::id());
        let dir = std::env::temp_dir().join(&label);
        std::fs::create_dir(&dir).unwrap();
        let endpoint = tmuxx_runtime::windows::endpoint(&label).unwrap();
        let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&endpoint)).unwrap();
        let fixture = Self {
            dir,
            label,
            endpoint,
            reference: RemoteReference::new(),
            host: Some(host),
        };
        for socket in ["c", "native"] {
            for (session, window) in [
                (b"main".as_slice(), b"main".as_slice()),
                (VALID_SESSION, "window\u{fffd}".as_bytes()),
            ] {
                let mut args = vec![b"new".as_slice(), b"-d", b"-s", session, b"-n", window];
                if socket == "c" {
                    args.push(b"sleep 120");
                } else {
                    args.extend([b"python".as_slice(), b"-c", b"import time; time.sleep(120)"]);
                }
                assert_success(fixture.run(socket, &args), b"");
            }
        }
        fixture
    }
    fn run(&self, socket: &str, args: &[&[u8]]) -> Output {
        if socket == "c" {
            return self.reference.output(args, &[]);
        }
        let mut bridge = OwnedChild(Some(
            Command::new(EXE)
                .arg("--native-stdio")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .spawn()
                .unwrap(),
        ));
        let mut input = bridge.0.as_mut().unwrap().stdin.take().unwrap();
        let stdout = bridge.0.as_mut().unwrap().stdout.take().unwrap();
        let mut stderr = bridge.0.as_mut().unwrap().stderr.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(stdout);
            while let Ok(Some(record)) = tmuxx_runtime::read_record(&mut output, 32 * 1024 * 1024) {
                if sender.send(record).is_err() {
                    break;
                }
            }
        });
        serde_json::to_writer(
            &mut input,
            &Hello {
                instance: Some(self.label.clone()),
                cwd: Some(self.dir.to_str().unwrap().into()),
                ..Hello::default()
            },
        )
        .unwrap();
        input.write_all(b"\n").unwrap();
        let hello: Hello =
            serde_json::from_slice(&receiver.recv_timeout(Duration::from_secs(5)).unwrap())
                .unwrap();
        assert!(hello.instance_id.is_some());
        serde_json::to_writer(
            &mut input,
            &Request {
                request_id: 1,
                argv: args.iter().map(|arg| arg.to_vec()).collect(),
                stdin: None,
                subscribe: false,
                control: false,
            },
        )
        .unwrap();
        input.write_all(b"\n").unwrap();
        let reply = loop {
            let record = receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("binary selector reply deadline");
            if let ServerMessage::Reply(reply) = serde_json::from_slice(&record).unwrap() {
                break reply;
            }
        };
        assert_eq!(reply.request_id, 1);
        drop(input);
        drop(bridge);
        reader.join().unwrap();
        let mut diagnostics = Vec::new();
        stderr.read_to_end(&mut diagnostics).unwrap();
        assert!(
            diagnostics.is_empty(),
            "bridge diagnostics: {diagnostics:?}"
        );
        let error = reply
            .error
            .map(|message| format!("{message}\n").into_bytes())
            .unwrap_or_default();
        Output {
            status: ExitStatus::from_raw(u32::from(!error.is_empty())),
            stdout: reply.output,
            stderr: error,
        }
    }
    fn start(&self, socket: &str, args: &[&[u8]]) -> Process {
        if socket == "c" {
            return Process::Reference(self.reference.control_open_args(&args[1..]));
        }
        Process::Native(OwnedChild(Some(
            Command::new(EXE)
                .arg("--native-endpoint")
                .arg(&self.endpoint)
                .args(args.iter().map(|arg| std::str::from_utf8(arg).unwrap()))
                .current_dir(&self.dir)
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )))
    }
    fn graph(&self, socket: &str) -> Vec<u8> {
        let output = self.run(socket, &[b"list-panes", b"-aF", GRAPH]);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        output.stdout
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(host) = self.host.take() {
            host.shutdown().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn assert_success(output: Output, bytes: &[u8]) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, bytes, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

struct Case {
    args: Vec<&'static [u8]>,
    c_failed: bool,
    c_output: Vec<u8>,
    rust_error: Vec<u8>,
}

impl Case {
    fn new(args: Vec<&'static [u8]>, flag: char, c_failed: bool, c_output: Vec<u8>) -> Self {
        let name = tmuxx_command::registry::find(args[0]).unwrap().name;
        Self {
            args,
            c_failed,
            c_output,
            rust_error: format!("command {name}: -{flag} target is not valid UTF-8\n").into_bytes(),
        }
    }
}

fn cases() -> Vec<Case> {
    let session_error = b"can't find session: replace\xff\n".to_vec();
    let window_error = b"can't find window: window\xff\n".to_vec();
    let mut cases = vec![
        Case::new(
            vec![b"has", b"-t", BAD_SESSION],
            't',
            true,
            session_error.clone(),
        ),
        Case::new(
            vec![b"has", b"-t", b"replace\xc0\xaf"],
            't',
            true,
            b"can't find session: replace\xc0\xaf\n".to_vec(),
        ),
        Case::new(
            vec![b"list-windows", b"-t", BAD_SESSION, b"-F", GRAPH],
            't',
            true,
            session_error.clone(),
        ),
        Case::new(
            vec![
                b"list-panes",
                b"-t",
                b"replace\xef\xbf\xbd:window\xff",
                b"-F",
                GRAPH,
            ],
            't',
            true,
            window_error.clone(),
        ),
        Case::new(
            vec![
                b"display",
                b"-pt",
                BAD_SESSION,
                b"#{session_name}|#{window_name}|#{pane_id}",
            ],
            't',
            false,
            b"||\n".to_vec(),
        ),
        Case::new(
            vec![
                b"rename-window",
                b"-t",
                b"replace\xef\xbf\xbd:window\xff",
                b"never",
            ],
            't',
            true,
            window_error,
        ),
        Case::new(
            vec![b"kill-session", b"-t", BAD_SESSION],
            't',
            true,
            session_error.clone(),
        ),
        Case::new(
            vec![b"switch-client", b"-c", b"client\xff", b"-t", b"main"],
            'c',
            true,
            b"can't find client: client\xff\n".to_vec(),
        ),
        Case::new(
            vec![b"refresh-client", b"-t", b"client\xff"],
            't',
            true,
            b"can't find client: client\xff\n".to_vec(),
        ),
        Case::new(
            vec![b"display", b"-pc", b"client\xff", b"canfail-client"],
            'c',
            false,
            b"canfail-client\n".to_vec(),
        ),
    ];
    for name in [
        b"join-pane".as_slice(),
        b"move-pane",
        b"swap-pane",
        b"break-pane",
        b"link-window",
        b"move-window",
        b"swap-window",
    ] {
        cases.push(Case::new(
            vec![name, b"-s", BAD_SOURCE, b"-t", b"main:0"],
            's',
            true,
            session_error.clone(),
        ));
    }
    for name in [b"link-window".as_slice(), b"move-window"] {
        cases.push(Case::new(
            vec![name, b"-s", b"main:0", b"-t", b"replace\xff:9"],
            't',
            true,
            session_error.clone(),
        ));
    }
    cases
}

#[test]
#[ignore = "requires pinned real C reference and native pinned ConPTY"]
fn malformed_selectors_preserve_graph_and_binary_bridge_bytes() {
    let fixture = Fixture::new("argv");
    std::fs::write(fixture.dir.join("input"), b"file\0\xff\n").unwrap();
    fixture.reference.put(b"input", b"file\0\xff\n");
    for socket in ["c", "native"] {
        let before = fixture.graph(socket);
        assert_success(fixture.run(socket, &[b"has", b"-t", VALID_SESSION]), b"");
        assert_success(
            fixture.run(
                socket,
                &[
                    b"display",
                    b"-pt",
                    VALID_WINDOW,
                    b"#{session_name}|#{window_name}",
                ],
            ),
            "replace\u{fffd}|window\u{fffd}\n".as_bytes(),
        );
        // Only the effective repeated selector is looked up by C.
        assert_success(
            fixture.run(socket, &[b"has", b"-t", BAD_SESSION, b"-t", VALID_SESSION]),
            b"",
        );
        for case in cases() {
            let output = fixture.run(socket, &case.args);
            let failed = socket != "c" || case.c_failed;
            let expected = match socket {
                "c" => case.c_output.clone(),
                "native" => case.rust_error.clone(),
                _ => case.rust_error.clone(),
            };
            assert_eq!(
                output.status.code(),
                Some(i32::from(failed)),
                "{socket}: {:?}: {output:?}",
                case.args
            );
            assert_eq!(
                if failed {
                    &output.stderr
                } else {
                    &output.stdout
                },
                &expected,
                "{socket}: {:?}",
                case.args
            );
            assert!(
                if failed {
                    output.stdout.is_empty()
                } else {
                    output.stderr.is_empty()
                },
                "{output:?}"
            );
            assert_eq!(fixture.graph(socket), before, "{socket}: {:?}", case.args);
        }
        assert_success(
            fixture.run(socket, &[b"display", b"-p", b"literal\xff"]),
            b"literal\xff\n",
        );
        assert_success(fixture.run(socket, &[b"load-buffer", b"input"]), b"");
        assert_success(
            fixture.run(socket, &[b"save-buffer", b"-"]),
            b"file\0\xff\n",
        );
        assert_success(
            fixture.run(
                socket,
                &[b"set-option", b"-g", b"@selector-probe", b"initial"],
            ),
            b"",
        );
        let output = fixture.run(
            socket,
            &[
                b"set-option",
                b"-g",
                b"@selector-probe",
                b"before",
                b";",
                b"has-session",
                b"-t",
                BAD_SESSION,
                b";",
                b"set-option",
                b"-g",
                b"@selector-probe",
                b"after",
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(
            output.stderr,
            match socket {
                "c" => b"can't find session: replace\xff\n".to_vec(),
                "native" => b"command has-session: -t target is not valid UTF-8\n".to_vec(),
                _ => b"command has-session: -t target is not valid UTF-8\n".to_vec(),
            }
        );
        assert_success(
            fixture.run(socket, &[b"show-option", b"-gv", b"@selector-probe"]),
            b"before\n",
        );
    }
}

fn control_line(args: &[&[u8]]) -> Vec<u8> {
    let mut line = Vec::new();
    for arg in args {
        if !line.is_empty() {
            line.push(b' ');
        }
        line.push(b'"');
        for &byte in *arg {
            match byte {
                b'\\' | b'"' | b'$' | b'~' => line.extend([b'\\', byte]),
                0..=31 | 127..=255 => line.extend(format!("\\{byte:03o}").bytes()),
                _ => line.push(byte),
            }
        }
        line.push(b'"');
    }
    line.push(b'\n');
    line
}

#[test]
#[ignore = "requires pinned real C reference and native pinned ConPTY"]
fn malformed_selectors_have_exact_control_errors_and_leave_attachment_usable() {
    let fixture = Fixture::new("control");
    for socket in ["c", "native"] {
        let before = fixture.graph(socket);
        let mut cases = cases();
        cases.push(Case::new(
            vec![b"detach-client", b"-s", BAD_SESSION],
            's',
            false,
            Vec::new(),
        ));
        let child = fixture.start(socket, &[b"-C", b"attach", b"-t", b"main"]);
        let mut script = Vec::new();
        let mut expected = vec![(false, Vec::new())]; // Initial attachment.
        for case in cases {
            script.extend(control_line(&case.args));
            expected.push(if socket == "c" {
                (case.c_failed, case.c_output)
            } else {
                (true, case.rust_error)
            });
            script.extend(control_line(&[b"list-panes", b"-aF", GRAPH]));
            expected.push((false, before.clone()));
        }
        script.extend(control_line(&[
            b"display",
            b"-pt",
            VALID_WINDOW,
            b"#{session_name}|#{window_name}",
        ]));
        expected.push((
            false,
            "replace\u{fffd}|window\u{fffd}\n".as_bytes().to_vec(),
        ));
        script.extend(control_line(&[
            b"set-option",
            b"-g",
            b"@selector-probe",
            b"initial",
        ]));
        expected.push((false, Vec::new()));
        script.extend(b"set-option -g @selector-probe before ; has-session -t \"replace\\377\" ; set-option -g @selector-probe after\n");
        expected.push((false, Vec::new()));
        if socket == "c" {
            expected.push((true, b"can't find session: replace\xff\n".to_vec()));
        } else {
            expected.push((
                true,
                b"command has-session: -t target is not valid UTF-8\n".to_vec(),
            ));
        }
        script.extend(control_line(&[b"show-option", b"-gv", b"@selector-probe"]));
        expected.push((false, b"before\n".to_vec()));
        script.extend(b"if-shell -F 0 { kill-session -t \"replace\\377\" }\n");
        expected.push((false, Vec::new()));
        script.extend(control_line(&[b"detach-client"]));
        expected.push((false, Vec::new()));
        let output = child.finish(&script);
        assert!(output.status.success(), "{socket}: {output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let mut decoder = Decoder::default();
        let events = decoder.feed(&output.stdout).unwrap();
        decoder.finish().unwrap();
        let mut replies = Vec::new();
        let mut bytes = Vec::new();
        for event in events {
            match event {
                Event::CommandOutput(line) => {
                    bytes.extend(line);
                    bytes.push(b'\n');
                }
                Event::End { failed, .. } => replies.push((failed, std::mem::take(&mut bytes))),
                _ => {}
            }
        }
        assert_eq!(replies.len(), expected.len(), "{socket}");
        for (index, (actual, expected)) in replies.iter().zip(&expected).enumerate() {
            assert_eq!(actual, expected, "{socket}: reply {index}");
        }
        assert_eq!(fixture.graph(socket), before);
    }
}
