#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Server {
    program: PathBuf,
    socket: PathBuf,
}

impl Server {
    fn new(program: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            program: program.into(),
            socket: PathBuf::from(format!(
                "/tmp/tx-default-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(["-f", "/dev/null", "-S"])
            .arg(&self.socket)
            .env_remove("TMUX")
            .env_remove("TMUXX_ENDPOINT")
            .env_remove("SHELL")
            .env_remove("VISUAL")
            .env_remove("EDITOR");
        command
    }

    fn start(&self, shell: Option<&str>, visual: Option<&str>, editor: Option<&str>) -> Output {
        let mut command = self.command();
        for (key, value) in [("SHELL", shell), ("VISUAL", visual), ("EDITOR", editor)] {
            if let Some(value) = value {
                command.env(key, value);
            }
        }
        command
            .args([
                "new-session",
                "-d",
                "-s",
                "main",
                "sleep 120",
                ";",
                "display-message",
                "-p",
                "-t",
                "main",
                "#{default-shell}|#{editor}|#{status-keys}|#{mode-keys}",
            ])
            .output()
            .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
    }
}

#[test]
#[ignore = "requires separately selected C tmux executable in TMUXX_C_TMUX"]
fn startup_shell_and_editor_defaults_match_c() {
    let oracle = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
    for (shell, visual, editor, expected_editor, keys) in [
        (None, None, None, "/usr/bin/vi", "emacs"),
        (Some(""), None, Some("vi"), "vi", "vi"),
        (Some("sh"), None, Some("/bin/emacs"), "/bin/emacs", "emacs"),
        (
            Some("/missing/tmuxx-shell"),
            None,
            Some("/usr/bin/nvim -f"),
            "/usr/bin/nvim -f",
            "vi",
        ),
        (oracle.to_str(), None, Some("vim"), "vim", "vi"),
        (Some("/bin/sh"), Some(""), Some("vim"), "", "emacs"),
        (
            Some("/bin/sh"),
            Some("/vi/path/emacs"),
            Some("vim"),
            "/vi/path/emacs",
            "emacs",
        ),
        (Some("/bin/sh"), Some("/bin/VI"), None, "/bin/VI", "emacs"),
        (
            Some("/bin/sh"),
            Some("/bin/view --readonly"),
            Some("emacs"),
            "/bin/view --readonly",
            "vi",
        ),
    ] {
        let c = Server::new(&oracle);
        let rust = Server::new(Path::new(env!("CARGO_BIN_EXE_tmuxx")));
        let expected = c.start(shell, visual, editor);
        let actual = rust.start(shell, visual, editor);
        let case = (shell, visual, editor);
        assert!(
            expected.status.success(),
            "C {case:?}: {}",
            String::from_utf8_lossy(&expected.stderr)
        );
        assert_eq!(
            actual.status.code(),
            expected.status.code(),
            "status {case:?}: {}",
            String::from_utf8_lossy(&actual.stderr)
        );
        assert_eq!(actual.stdout, expected.stdout, "stdout {case:?}");
        assert_eq!(actual.stderr, expected.stderr, "stderr {case:?}");
        assert!(
            actual
                .stdout
                .ends_with(format!("|{expected_editor}|{keys}|{keys}\n").as_bytes()),
            "defaults {case:?}: {}",
            String::from_utf8_lossy(&actual.stdout)
        );
        if shell == Some("/bin/sh") {
            assert!(actual.stdout.starts_with(b"/bin/sh|"));
        }
    }
}

#[test]
#[ignore = "requires current C tmux with client environment format loops"]
fn client_environment_format_uses_request_environment() {
    let oracle = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
    let c = Server::new(&oracle);
    let rust = Server::new(Path::new(env!("CARGO_BIN_EXE_tmuxx")));
    for server in [&c, &rust] {
        assert!(server.start(Some("/bin/sh"), None, None).status.success());
    }
    let query = |server: &Server, value: &str| {
        server.command().env("PARITY_REQUEST_ALPHA", value)
            .env("PARITY_REQUEST_EMPTY", "")
            .args(["display-message", "-p", "-t", "main", "#{V/c:#{?#{m:PARITY_REQUEST_*,#{environ_name}},#{environ_name}=#{environ_value}:#{environ_hidden}:#{environ_removed}|,}}"])
            .output().unwrap()
    };
    for value in ["first-client", "next-client"] {
        let expected = query(&c, value);
        let actual = query(&rust, value);
        assert!(
            expected.status.success(),
            "{}",
            String::from_utf8_lossy(&expected.stderr)
        );
        assert_eq!(actual.status.code(), expected.status.code());
        assert_eq!(actual.stdout, expected.stdout);
        assert_eq!(actual.stderr, expected.stderr);
        assert_eq!(
            actual.stdout,
            format!("PARITY_REQUEST_ALPHA={value}:0:0|PARITY_REQUEST_EMPTY=:0:0|\n").as_bytes()
        );
    }
}

#[test]
#[ignore = "requires C tmux, real child processes and control clients"]
fn child_environment_overrides_and_attach_updates_match_c() {
    use std::time::{Duration, Instant};
    let oracle = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
    let mut observations = Vec::new();
    for program in [oracle.as_path(), Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        let server = Server::new(program);
        assert!(server.start(Some("/bin/sh"), None, None).status.success());
        let run = |args: &[&str]| {
            let output = server.command().args(args).output().unwrap();
            assert!(output.status.success(), "{program:?} {args:?}: {output:?}");
            output.stdout
        };
        for args in [
            vec!["setenv", "-g", "PARITY_INHERITED", "global"],
            vec!["setenv", "-g", "PARITY_MASK", "global"],
            vec!["setenv", "-g", "PARITY_SECRET", "global"],
            vec!["setenv", "-r", "PARITY_MASK"],
            vec!["setenv", "-h", "PARITY_SECRET", "secret"],
            vec![
                "set",
                "-g",
                "update-environment",
                "PARITY_UPDATE PARITY_MISSING PARITY_GLOB_*",
            ],
            vec!["setenv", "PARITY_UPDATE", "before"],
            vec!["setenv", "PARITY_MISSING", "before"],
        ] {
            run(&args);
        }
        let script = "printf '%s\\n' ENV \"${PARITY_INHERITED-unset}\" \"${PARITY_MASK-unset}\" \"${PARITY_SECRET-unset}\" \"$PARITY_EXTRA\" \"$TERM\" \"$SHELL\" \"$PATH\"; exec /bin/sleep 120";
        let mut outputs = Vec::new();
        let mut pane = String::new();
        for name in ["new-window", "split-window", "respawn-pane"] {
            let mut command = server.command();
            command.env("PATH", "/client/path").arg(name);
            if name == "respawn-pane" {
                command.args(["-k", "-t", &pane]);
            } else {
                command.args(["-dP", "-F", "#{pane_id}"]);
            }
            let output = command
                .args([
                    "-e",
                    "PARITY_INHERITED",
                    "-e",
                    "PARITY_EXTRA=first",
                    "-e",
                    "PARITY_EXTRA=last",
                    "-e",
                    "TERM=custom-term",
                    "-e",
                    "SHELL=/not-the-shell",
                    "-e",
                    "PATH=/not-the-client-path",
                    script,
                ])
                .output()
                .unwrap();
            assert!(output.status.success(), "{program:?} {name}: {output:?}");
            if name != "respawn-pane" {
                pane = String::from_utf8(output.stdout).unwrap().trim().to_owned();
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let capture = run(&["capture-pane", "-p", "-t", &pane, "-S", "0", "-E", "7"]);
                if capture.starts_with(b"ENV\n") {
                    assert_eq!(
                        capture,
                        b"ENV\nglobal\nunset\nunset\nlast\ncustom-term\n/bin/sh\n/client/path\n",
                        "{program:?} {name}"
                    );
                    outputs.push(capture);
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "child environment was not printed"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        for skip in [true, false] {
            let mut command = server.command();
            command
                .env("PARITY_UPDATE", "attached")
                .env("PARITY_GLOB_VALUE", "glob")
                .env_remove("PARITY_MISSING");
            command.args(["-C", "attach-session", "-t", "main"]);
            if skip {
                command.arg("-E");
            }
            let output = command.args([";", "detach-client"]).output().unwrap();
            assert!(output.status.success(), "{program:?}: {output:?}");
            for (name, expected) in [
                (
                    "PARITY_UPDATE",
                    if skip {
                        "PARITY_UPDATE=before\n"
                    } else {
                        "PARITY_UPDATE=attached\n"
                    },
                ),
                (
                    "PARITY_MISSING",
                    if skip {
                        "PARITY_MISSING=before\n"
                    } else {
                        "-PARITY_MISSING\n"
                    },
                ),
            ] {
                let value = run(&["showenv", "-t", "main", name]);
                assert_eq!(value, expected.as_bytes());
                outputs.push(value);
            }
        }
        outputs.push(run(&["showenv", "-t", "main", "PARITY_GLOB_VALUE"]));
        assert_eq!(outputs.last().unwrap(), b"PARITY_GLOB_VALUE=glob\n");
        run(&["set", "-gw", "remain-on-exit", "on"]);
        for (flags, value, override_value, caller, expected) in [
            ("-gu", None, None, None, Some("/usr/bin:/bin")),
            ("-gr", None, None, None, None),
            ("-gh", Some("/hidden/path"), None, None, None),
            (
                "-gh",
                Some("/hidden/path"),
                Some("/pane/path"),
                None,
                Some("/pane/path"),
            ),
            (
                "-gr",
                None,
                Some("/pane/path"),
                Some("/client/path"),
                Some("/client/path"),
            ),
        ] {
            run(&["setenv", "-u", "PATH"]);
            let mut args = vec!["setenv", flags, "PATH"];
            args.extend(value);
            run(&args);
            let mut command = server.command();
            command.env_remove("PATH");
            if let Some(path) = caller {
                command.env("PATH", path);
            }
            command.args(["new-window", "-dP", "-F", "#{pane_id}"]);
            if let Some(path) = override_value {
                command.args(["-e", &format!("PATH={path}")]);
            }
            let output = command.args(["/usr/bin/env", "--"]).output().unwrap();
            assert!(output.status.success(), "{program:?}: {output:?}");
            let pane = String::from_utf8(output.stdout).unwrap().trim().to_owned();
            let deadline = Instant::now() + Duration::from_secs(5);
            while run(&["display", "-pt", &pane, "#{pane_dead}"]) != b"1\n" {
                assert!(Instant::now() < deadline, "environment child did not exit");
                std::thread::sleep(Duration::from_millis(5));
            }
            let capture = run(&["capture-pane", "-p", "-t", &pane, "-S", "-", "-E", "-"]);
            let path = capture
                .split(|b| *b == b'\n')
                .find_map(|line| line.strip_prefix(b"PATH="));
            assert_eq!(
                path,
                expected.map(str::as_bytes),
                "{program:?} {flags} {override_value:?} {caller:?}"
            );
            outputs.push(path.unwrap_or_default().to_vec());
        }
        observations.push(outputs);
    }
    assert_eq!(observations[0], observations[1]);
}

#[test]
#[ignore = "requires C tmux and real pane processes"]
fn implementation_identity_is_explicit_and_pane_overrides_take_precedence() {
    use std::time::{Duration, Instant};
    let oracle = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
    let c_version = Command::new(&oracle).arg("-V").output().unwrap();
    assert!(c_version.status.success());
    let c_version = String::from_utf8(c_version.stdout).unwrap();
    let c_version = c_version.trim().strip_prefix("tmux ").unwrap();
    for (program, identity, version) in [
        (oracle.as_path(), "tmux", c_version),
        (
            Path::new(env!("CARGO_BIN_EXE_tmuxx")),
            "tmuxx",
            env!("CARGO_PKG_VERSION"),
        ),
    ] {
        let server = Server::new(program);
        assert!(server.start(Some("/bin/sh"), None, None).status.success());
        let run = |args: &[&str]| {
            let output = server.command().args(args).output().unwrap();
            assert!(output.status.success(), "{program:?} {args:?}: {output:?}");
            output.stdout
        };
        assert_eq!(
            run(&["display-message", "-p", "#{version}"]),
            format!("{version}\n").as_bytes()
        );
        for overrides in [false, true] {
            let mut args = vec!["new-window", "-dP", "-F", "#{pane_id}"];
            if overrides {
                args.extend([
                    "-e",
                    "TERM_PROGRAM=host",
                    "-e",
                    "TERM_PROGRAM_VERSION=custom",
                ]);
            }
            args.push("printf 'IDENTITY\\n%s\\n%s\\n' \"$TERM_PROGRAM\" \"$TERM_PROGRAM_VERSION\"; exec /bin/sleep 120");
            let pane = String::from_utf8(run(&args)).unwrap();
            let expected = if overrides {
                "IDENTITY\nhost\ncustom\n".to_owned()
            } else {
                format!("IDENTITY\n{identity}\n{version}\n")
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let capture = run(&[
                    "capture-pane",
                    "-p",
                    "-t",
                    pane.trim(),
                    "-S",
                    "0",
                    "-E",
                    "2",
                ]);
                if capture.starts_with(b"IDENTITY\n") {
                    assert_eq!(capture, expected.as_bytes());
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "identity was not printed: {capture:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
#[ignore = "requires C tmux and real control clients and pane processes"]
fn control_transport_path_override_depends_on_attachment_like_c() {
    use std::time::{Duration, Instant};
    let oracle = PathBuf::from(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"));
    for program in [oracle.as_path(), Path::new(env!("CARGO_BIN_EXE_tmuxx"))] {
        let server = Server::new(program);
        assert!(server.start(Some("/bin/sh"), None, None).status.success());
        let script = "printf 'PATH\\n%s\\n' \"$PATH\"; exec /bin/sleep 120";
        for args in [
            vec![
                "-C",
                "new-session",
                "-d",
                "-s",
                "detached",
                "-e",
                "PATH=/session/path",
                script,
            ],
            vec![
                "-C",
                "new-session",
                "-s",
                "attached",
                "-e",
                "PATH=/session/path",
                script,
                ";",
                "new-window",
                "-d",
                "-t",
                "attached:1",
                "-e",
                "PATH=/pane/path",
                script,
                ";",
                "detach-client",
            ],
            vec![
                "-C",
                "new-window",
                "-d",
                "-t",
                "main:2",
                "-e",
                "PATH=/pane/path",
                script,
            ],
            vec![
                "-C",
                "attach",
                "-t",
                "main",
                ";",
                "respawn-pane",
                "-k",
                "-t",
                "main:2",
                "-e",
                "PATH=/attached/respawn",
                script,
                ";",
                "split-window",
                "-d",
                "-t",
                "main:0",
                "-e",
                "PATH=/attached/split",
                script,
                ";",
                "detach-client",
            ],
        ] {
            let output = server
                .command()
                .env("PATH", "/client/path")
                .args(&args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{program:?} {args:?}: {output:?}");
        }
        for (target, expected) in [
            ("detached:0", "/client/path"),
            ("attached:0", "/client/path"),
            ("attached:1", "/pane/path"),
            ("main:2", "/attached/respawn"),
            ("main:0.1", "/attached/split"),
        ] {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let output = server
                    .command()
                    .args(["capture-pane", "-pt", target, "-S", "0", "-E", "1"])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{program:?}: {output:?}");
                if output.stdout.starts_with(b"PATH\n") {
                    assert_eq!(
                        output.stdout,
                        format!("PATH\n{expected}\n").as_bytes(),
                        "{program:?} {target}"
                    );
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "no child PATH for {program:?} {target}: {output:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}
