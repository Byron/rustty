#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    dir: PathBuf,
    c: PathBuf,
    host: Option<tmuxx_runtime::ServerHandle>,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("tmuxx-source-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
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
            let out = fixture.run(
                &fixture.c,
                socket,
                &["new-session", "-d", "-s", "test", "sleep 120"],
                b"",
            );
            assert!(out.status.success(), "{out:?}");
        }
        fixture
    }
    fn start(&self, client: &Path, socket: &str, args: &[&str]) -> Child {
        let mut command = Command::new(client);
        command.args(["-f", "/dev/null"]);
        command
            .arg(if socket == "native" {
                "--native-endpoint"
            } else {
                "-S"
            })
            .arg(self.dir.join(socket));
        command
            .args(args)
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
    fn run(&self, client: &Path, socket: &str, args: &[&str], input: &[u8]) -> Output {
        let mut child = self.start(client, socket, args);
        child.stdin.take().unwrap().write_all(input).unwrap();
        wait(child)
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
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!(
                "source client timed out: {:?}",
                child.wait_with_output().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires explicit C reference and real Unix sockets/PTYs"]
fn source_stdin_all_four_pairings_preserve_files_flags_groups_and_preparation() {
    let fixture = Fixture::new();
    let rust = Path::new(env!("CARGO_BIN_EXE_tmuxx"));
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
        std::fs::write(fixture.dir.join(name), text).unwrap();
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
        for (client, socket) in [
            (fixture.c.as_path(), "c"),
            (fixture.c.as_path(), "rust"),
            (rust, "c"),
            (rust, "rust"),
        ] {
            for setup in [
                vec!["set-environment", "-g", "SEEN", ""],
                vec!["set-environment", "-g", "SOURCE_PATH", "before"],
                vec!["set-option", "-g", "@source", "before"],
            ] {
                let result = fixture.run(client, socket, &setup, b"");
                assert!(result.status.success(), "{setup:?}: {result:?}");
            }
            let output = fixture.run(client, socket, &args, input);
            let state = fixture.run(client, socket, &["show-environment", "-g", "SEEN"], b"");
            let observation = (
                output.status.code(),
                output.stdout,
                output.stderr,
                state.stdout,
            );
            if let Some(expected) = &expected {
                assert_eq!(&observation, expected, "{client:?}→{socket}: {args:?}");
            } else {
                expected = Some(observation);
            }
        }
    }
    let native = fixture.run(
        rust,
        "native",
        &["source", "-", ";", "display", "-p", "after-native"],
        b"display -p native-stdin\n",
    );
    assert!(native.status.success(), "{native:?}");
    assert_eq!(native.stdout, b"native-stdin\nafter-native\n");
    // A waiting source must not hold the owner loop or prepare its prefix twice.
    for (client, socket) in [
        (fixture.c.as_path(), "c"),
        (fixture.c.as_path(), "rust"),
        (rust, "c"),
        (rust, "rust"),
    ] {
        assert!(
            fixture
                .run(client, socket, &["set-environment", "-g", "SEEN", ""], b"")
                .status
                .success()
        );
        let mut waiting = fixture.start(client, socket, &["source", "before", "-", "after"]);
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let result = fixture.run(client, socket, &["show-environment", "-g", "SEEN"], b"");
            if result.stdout == b"SEEN=x\n" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "prefix was not prepared: {result:?}"
            );
        }
        assert!(
            fixture
                .run(
                    client,
                    socket,
                    &["set-environment", "-g", "SEEN", "external"],
                    b""
                )
                .status
                .success()
        );
        waiting.stdin.take().unwrap().write_all(stdin).unwrap();
        let output = wait(waiting);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            output.stdout, b"before=x\nstdin=externaly\nafter=externaly\n",
            "{client:?}→{socket}"
        );
    }
}
