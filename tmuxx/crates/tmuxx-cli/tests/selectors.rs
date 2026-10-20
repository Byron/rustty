#![cfg(unix)]

use std::io::Write;
use std::os::unix::{ffi::OsStrExt, fs::DirBuilderExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tmuxx_protocol::{Decoder, Event};

const VALID_SESSION: &[u8] = "replace\u{fffd}".as_bytes();
const VALID_WINDOW: &[u8] = "replace\u{fffd}:window\u{fffd}".as_bytes();
const BAD_SESSION: &[u8] = b"replace\xff";
const BAD_SOURCE: &[u8] = b"replace\xff:window\xef\xbf\xbd";
const GRAPH: &[u8] = b"#{session_name}|#{window_id}|#{window_name}|#{pane_id}";

struct Fixture {
    dir: PathBuf,
    c: PathBuf,
    host: Option<tmuxx_runtime::ServerHandle>,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tmuxx-selectors-{}-{label}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let mut options = tmuxx_runtime::Options::new(dir.join("native"));
        options.tmux_endpoint = Some(dir.join("rust"));
        let fixture = Self {
            c: std::env::var_os("TMUXX_C_TMUX")
                .expect("set TMUXX_C_TMUX")
                .into(),
            host: Some(tmuxx_runtime::start(options).unwrap()),
            dir,
        };
        for socket in ["c", "rust"] {
            for (session, window) in [
                (b"main".as_slice(), b"main".as_slice()),
                (VALID_SESSION, "window\u{fffd}".as_bytes()),
            ] {
                assert_success(
                    fixture.run(
                        &fixture.c,
                        socket,
                        &[b"new", b"-d", b"-s", session, b"-n", window, b"sleep 120"],
                    ),
                    b"",
                );
            }
        }
        fixture
    }

    fn start(&self, client: &Path, socket: &str, args: &[&[u8]]) -> Child {
        Command::new(client)
            .args(["-f", "/dev/null"])
            .arg(if socket == "native" {
                "--native-endpoint"
            } else {
                "-S"
            })
            .arg(self.dir.join(socket))
            .args(args.iter().map(|arg| std::ffi::OsStr::from_bytes(arg)))
            .env_remove("TMUX")
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn run(&self, client: &Path, socket: &str, args: &[&[u8]]) -> Output {
        let mut child = self.start(client, socket, args);
        drop(child.stdin.take());
        wait(child)
    }

    fn graph(&self, client: &Path, socket: &str) -> Vec<u8> {
        let output = self.run(client, socket, &[b"list-panes", b"-aF", GRAPH]);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        output.stdout
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new(&self.c)
            .arg("-S")
            .arg(self.dir.join("c"))
            .arg("kill-server")
            .output();
        if let Some(host) = self.host.take() {
            let _ = host.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!(
                "selector client timed out: {:?}",
                child.wait_with_output().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
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
#[ignore = "requires explicit C reference and real Unix sockets/PTYs"]
fn malformed_selectors_preserve_graph_and_client_forwarding_in_all_pairings() {
    let fixture = Fixture::new("argv");
    let rust = Path::new(env!("CARGO_BIN_EXE_tmuxx"));
    std::fs::write(fixture.dir.join("input"), b"file\0\xff\n").unwrap();
    for (client, socket) in [
        (fixture.c.as_path(), "c"),
        (rust, "c"),
        (fixture.c.as_path(), "rust"),
        (rust, "rust"),
        (rust, "native"),
    ] {
        let before = fixture.graph(client, socket);
        assert_success(
            fixture.run(client, socket, &[b"has", b"-t", VALID_SESSION]),
            b"",
        );
        assert_success(
            fixture.run(
                client,
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
            fixture.run(
                client,
                socket,
                &[b"has", b"-t", BAD_SESSION, b"-t", VALID_SESSION],
            ),
            b"",
        );
        for case in cases() {
            let output = fixture.run(client, socket, &case.args);
            let failed = socket != "c" || case.c_failed;
            let expected = match socket {
                "c" => case.c_output.clone(),
                "native" => [b"tmuxx: ".as_slice(), &case.rust_error].concat(),
                _ => case.rust_error.clone(),
            };
            assert_eq!(
                output.status.code(),
                Some(i32::from(failed)),
                "{client:?}→{socket}: {:?}: {output:?}",
                case.args
            );
            assert_eq!(
                if failed {
                    &output.stderr
                } else {
                    &output.stdout
                },
                &expected,
                "{client:?}→{socket}: {:?}",
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
            assert_eq!(
                fixture.graph(client, socket),
                before,
                "{client:?}→{socket}: {:?}",
                case.args
            );
        }
        assert_success(
            fixture.run(client, socket, &[b"display", b"-p", b"literal\xff"]),
            b"literal\xff\n",
        );
        assert_success(
            fixture.run(client, socket, &[b"load-buffer", b"input"]),
            b"",
        );
        assert_success(
            fixture.run(client, socket, &[b"save-buffer", b"-"]),
            b"file\0\xff\n",
        );
        assert_success(
            fixture.run(
                client,
                socket,
                &[b"set-option", b"-g", b"@selector-probe", b"initial"],
            ),
            b"",
        );
        let output = fixture.run(
            client,
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
                "native" => b"tmuxx: command has-session: -t target is not valid UTF-8\n".to_vec(),
                _ => b"command has-session: -t target is not valid UTF-8\n".to_vec(),
            }
        );
        assert_success(
            fixture.run(
                client,
                socket,
                &[b"show-option", b"-gv", b"@selector-probe"],
            ),
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
#[ignore = "requires explicit C reference and real Unix sockets/PTYs"]
fn malformed_selectors_have_exact_control_errors_and_leave_attachment_usable() {
    let fixture = Fixture::new("control");
    let rust = Path::new(env!("CARGO_BIN_EXE_tmuxx"));
    for (client, socket) in [
        (fixture.c.as_path(), "c"),
        (rust, "c"),
        (fixture.c.as_path(), "rust"),
        (rust, "rust"),
        (rust, "native"),
    ] {
        let before = fixture.graph(client, socket);
        let mut cases = cases();
        cases.push(Case::new(
            vec![b"detach-client", b"-s", BAD_SESSION],
            's',
            false,
            Vec::new(),
        ));
        let mut child = fixture.start(client, socket, &[b"-C", b"attach", b"-t", b"main"]);
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
        child.stdin.take().unwrap().write_all(&script).unwrap();
        let output = wait(child);
        assert!(output.status.success(), "{client:?}→{socket}: {output:?}");
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
        assert_eq!(replies.len(), expected.len(), "{client:?}→{socket}");
        for (index, (actual, expected)) in replies.iter().zip(&expected).enumerate() {
            assert_eq!(actual, expected, "{client:?}→{socket}: reply {index}");
        }
        assert_eq!(fixture.graph(client, socket), before);
    }
}
