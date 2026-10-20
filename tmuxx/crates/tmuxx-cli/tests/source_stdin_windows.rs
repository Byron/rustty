#![cfg(windows)]

//! The complete source/stdin matrix runs on native Windows CLI/pipe/ConPTY and
//! the pinned real C CLI over no-PTY SSH. Unix IPC pairings retain their own suite.
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;

use remote_reference::{RemoteControl, RemoteReference};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const EXE: &str = env!("CARGO_BIN_EXE_tmuxx");

struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Fixture {
    directory: PathBuf,
    endpoint: PathBuf,
    host: Option<tmuxx_runtime::ServerHandle>,
    reference: RemoteReference,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("tmuxx-source-windows-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let endpoint =
            tmuxx_runtime::windows::endpoint(directory.file_name().unwrap().to_str().unwrap())
                .unwrap();
        let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&endpoint)).unwrap();
        let fixture = Self {
            directory,
            endpoint,
            host: Some(host),
            reference: RemoteReference::new(),
        };
        for remote in [false, true] {
            let args = if remote {
                vec!["new-session", "-d", "-s", "test", "sleep 120"]
            } else {
                vec![
                    "new-session",
                    "-d",
                    "-s",
                    "test",
                    "python",
                    "-c",
                    "import time; time.sleep(120)",
                ]
            };
            let output = fixture.run(remote, &args, b"");
            assert!(output.status.success(), "{output:?}");
        }
        fixture
    }

    fn start_native(&self, args: &[&str]) -> OwnedChild {
        OwnedChild(Some(
            Command::new(EXE)
                .arg("--native-endpoint")
                .arg(&self.endpoint)
                .args(args)
                .current_dir(&self.directory)
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }

    fn run(&self, remote: bool, args: &[&str], input: &[u8]) -> Output {
        if remote {
            self.reference.output(
                &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
                input,
            )
        } else {
            let mut child = self.start_native(args);
            child
                .0
                .as_mut()
                .unwrap()
                .stdin
                .take()
                .unwrap()
                .write_all(input)
                .unwrap();
            wait_native(child)
        }
    }

    fn expected_native_source_paths(&self, bytes: Vec<u8>) -> Vec<u8> {
        // Source -v and line errors identify canonical source filenames. The
        // two real hosts necessarily have different private roots/separators.
        // Project only these five exact fixture filename prefixes; every line
        // number, command byte, error, and unrelated path remains untouched.
        let remote = self.reference.directory();
        let native = std::fs::canonicalize(&self.directory).unwrap();
        let mut output = Vec::new();
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            let mut projected = false;
            for leaf in ["before", "after", "invalid", "groups", "format"] {
                let prefix = [remote.as_slice(), b"/", leaf.as_bytes(), b":"].concat();
                if let Some(rest) = line.strip_prefix(prefix.as_slice()) {
                    output.extend_from_slice(native.join(leaf).to_str().unwrap().as_bytes());
                    output.push(b':');
                    output.extend_from_slice(rest);
                    projected = true;
                    break;
                }
            }
            if !projected {
                output.extend_from_slice(line);
            }
        }
        output
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(host) = self.host.take() {
            host.shutdown().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn wait_native(mut owned: OwnedChild) -> Output {
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
            "native source client exceeded bounded wait"
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

fn wait_remote(child: RemoteControl, input: &[u8]) -> Output {
    assert!(child.write(input).status.success());
    assert!(child.finish_input().status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(code) = child.exited() {
            let mut output = child.read();
            use std::os::windows::process::ExitStatusExt;
            output.status = std::process::ExitStatus::from_raw(code as u32);
            return output;
        }
        assert!(
            Instant::now() < deadline,
            "C source client exceeded bounded wait"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires explicit pinned SSH C reference and native pinned ConPTY"]
fn source_stdin_preserves_complete_files_flags_groups_and_preparation_matrix() {
    let fixture = Fixture::new();
    for (name, text) in [
        ("before", "SEEN=\"${SEEN}x\"\ndisplay -p \"before=$SEEN\"\n"),
        ("after", "display -p \"after=$SEEN\"\n"),
        ("invalid", "display -p never\nunknown-command\n"),
        (
            "groups",
            "display -p first; rename-session -t absent ignored; display -p skipped\ndisplay -p second\n",
        ),
        ("format", "SOURCE_PATH=after\ndisplay -p formatted\n"),
    ] {
        std::fs::write(fixture.directory.join(name), text).unwrap();
        fixture.reference.put(name.as_bytes(), text.as_bytes());
    }
    let stdin = b"SEEN=\"${SEEN}y\"\ndisplay -p \"stdin=$SEEN\"\n";
    let cases = [
        (vec!["source", "-"], b"display -p '#{command}'\n".as_slice()),
        (
            vec![
                "source-file",
                "before",
                "-",
                "after",
                ";",
                "display",
                "-p",
                "following",
            ],
            stdin,
        ),
        (vec!["source", "-v", "before", "-", "after"], stdin),
        (vec!["source", "-nv", "before", "-", "after"], stdin),
        (
            vec!["source", "-q", "absent*", "before", "-", "after"],
            stdin,
        ),
        (
            vec!["source", "-F", "-t", "test:0.0", "#{@source}", "-", "after"],
            stdin,
        ),
        (
            vec!["source", "-v", "invalid", "-", "groups", "after"],
            stdin,
        ),
        (
            vec!["source", "-v", "before", "-", "after"],
            b"display -p never; unknown-command\n",
        ),
        (
            vec![
                "source",
                "missing",
                "before",
                "-",
                "after",
                ";",
                "display",
                "-p",
                "following",
            ],
            stdin,
        ),
        (vec!["source", "before", "-", "missing", "after"], stdin),
        (
            vec!["source", "-F", "format", "#{SOURCE_PATH}", "-"],
            b"display -p ending\n",
        ),
        (vec!["source", "-", "-", "after"], stdin),
        (vec!["source", "before", "-", "after"], b""),
    ];
    for (args, input) in cases {
        let mut expected = None;
        for remote in [true, false] {
            for setup in [
                vec!["set-environment", "-g", "SEEN", ""],
                vec!["set-environment", "-g", "SOURCE_PATH", "before"],
                vec!["set-option", "-g", "@source", "before"],
            ] {
                let result = fixture.run(remote, &setup, b"");
                assert!(result.status.success(), "{setup:?}: {result:?}");
            }
            let output = fixture.run(remote, &args, input);
            let state = fixture.run(remote, &["show-environment", "-g", "SEEN"], b"");
            assert!(state.status.success(), "{state:?}");
            let actual = (
                output.status.code(),
                output.stdout,
                output.stderr,
                state.stdout,
            );
            if let Some(expected) = &expected {
                assert_eq!(&actual, expected, "native Windows vs real C: {args:?}");
            } else {
                let diagnostic = fixture.expected_native_source_paths(actual.2);
                expected = Some((
                    actual.0,
                    fixture.expected_native_source_paths(actual.1),
                    diagnostic,
                    actual.3,
                ));
            }
        }
    }
    let native = fixture.run(
        false,
        &["source", "-", ";", "display", "-p", "after-native"],
        b"display -p native-stdin\n",
    );
    assert!(native.status.success(), "{native:?}");
    assert_eq!(native.stdout, b"native-stdin\nafter-native\n");

    // A held stdin must suspend only its own queue, prepare the prefix once,
    // and observe another client's environment mutation when input resumes.
    for remote in [true, false] {
        assert!(
            fixture
                .run(remote, &["set-environment", "-g", "SEEN", ""], b"")
                .status
                .success()
        );
        let args = ["source", "before", "-", "after"];
        let mut native_child = (!remote).then(|| fixture.start_native(&args));
        let remote_child = remote.then(|| {
            fixture
                .reference
                .command_open_args(&args.map(str::as_bytes))
        });
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let result = fixture.run(remote, &["show-environment", "-g", "SEEN"], b"");
            if result.stdout == b"SEEN=x\n" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "prefix was not prepared: {result:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            fixture
                .run(remote, &["set-environment", "-g", "SEEN", "external"], b"")
                .status
                .success()
        );
        let output = if let Some(child) = remote_child {
            wait_remote(child, stdin)
        } else {
            let mut child = native_child.take().unwrap();
            child
                .0
                .as_mut()
                .unwrap()
                .stdin
                .take()
                .unwrap()
                .write_all(stdin)
                .unwrap();
            wait_native(child)
        };
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            output.stdout, b"before=x\nstdin=externaly\nafter=externaly\n",
            "remote={remote}"
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}
