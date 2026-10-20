#![cfg(windows)]

#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;

use remote_reference::RemoteReference;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const EXE: &str = env!("CARGO_BIN_EXE_tmuxx");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Reference,
    Native,
}

struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn bounded(command: &mut Command) -> Output {
    let mut owned = OwnedChild(Some(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
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
    let deadline = Instant::now() + Duration::from_secs(12);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "native default command exceeded bounded wait"
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

fn python() -> &'static str {
    static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PYTHON.get_or_init(|| {
        let output = bounded(Command::new("python").args([
            "-c",
            "import sys; sys.stdout.buffer.write(sys.executable.encode('utf-8'))",
        ]));
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    })
}

fn native_shell() -> String {
    let mut text = vec![0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            text.as_mut_ptr(),
            text.len() as u32,
        )
    };
    assert!(length > 0 && (length as usize) < text.len());
    format!(
        "{}\\cmd.exe",
        String::from_utf16(&text[..length as usize]).unwrap()
    )
}

fn native_fallback_path() -> String {
    let shell = native_shell();
    let system = std::path::Path::new(&shell)
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    let mut text = vec![0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
            text.as_mut_ptr(),
            text.len() as u32,
        )
    };
    assert!(length > 0 && (length as usize) < text.len());
    format!(
        "{system};{}",
        String::from_utf16(&text[..length as usize]).unwrap()
    )
}

struct Server {
    target: Target,
    directory: PathBuf,
    label: String,
    owner: RefCell<Option<OwnedChild>>,
    reference: RefCell<Option<RemoteReference>>,
}
impl Server {
    fn new(target: Target) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let label = format!(
            "defaults-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::temp_dir().join(&label);
        std::fs::create_dir(&directory).unwrap();
        Self {
            target,
            directory,
            label,
            owner: RefCell::new(None),
            reference: RefCell::new(None),
        }
    }
    fn command(&self) -> Request<'_> {
        let mut request = Request {
            server: self,
            args: Vec::new(),
            environment: BTreeMap::new(),
        };
        for key in ["TMUX", "TMUXX_ENDPOINT", "SHELL", "VISUAL", "EDITOR"] {
            request.env_remove(key);
        }
        request
    }
    fn start(&self, shell: Option<&str>, visual: Option<&str>, editor: Option<&str>) -> Output {
        let native = native_shell();
        let shell = if self.target == Target::Native && shell == Some("/bin/sh") {
            Some(native.as_str())
        } else {
            shell
        };
        let environment = [
            (b"SHELL".as_slice(), shell.map(str::as_bytes)),
            (b"VISUAL", visual.map(str::as_bytes)),
            (b"EDITOR", editor.map(str::as_bytes)),
        ];
        if self.target == Target::Reference {
            *self.reference.borrow_mut() =
                Some(RemoteReference::new_with_environment(&environment));
        } else {
            let config = self.directory.join("empty.conf");
            std::fs::write(&config, []).unwrap();
            let mut command = Command::new(EXE);
            command
                .args(["-D", "-L", &self.label, "-f"])
                .arg(&config)
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            for (key, value) in environment {
                let key = std::str::from_utf8(key).unwrap();
                match value {
                    Some(value) => {
                        command.env(key, std::str::from_utf8(value).unwrap());
                    }
                    None => {
                        command.env_remove(key);
                    }
                }
            }
            *self.owner.borrow_mut() = Some(OwnedChild(Some(command.spawn().unwrap())));
            let endpoint = tmuxx_runtime::windows::endpoint(&self.label).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(mut stream) = tmuxx_runtime::windows::connect(&endpoint) {
                    tmuxx_runtime::native_handshake(&mut stream).unwrap();
                    break;
                }
                assert!(
                    self.owner
                        .borrow_mut()
                        .as_mut()
                        .unwrap()
                        .0
                        .as_mut()
                        .unwrap()
                        .try_wait()
                        .unwrap()
                        .is_none(),
                    "owner failed during defaults startup"
                );
                assert!(
                    Instant::now() < deadline,
                    "defaults owner readiness timeout"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.command()
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
        if self.owner.get_mut().is_some() {
            let _ = self.command().arg("kill-server").output();
        }
        self.owner.get_mut().take();
        self.reference.get_mut().take();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Request<'a> {
    server: &'a Server,
    args: Vec<String>,
    environment: BTreeMap<String, Option<String>>,
}
impl Request<'_> {
    fn arg(&mut self, arg: impl AsRef<str>) -> &mut Self {
        self.args.push(arg.as_ref().into());
        self
    }
    fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().into()));
        self
    }
    fn env(&mut self, key: &str, value: &str) -> &mut Self {
        self.environment.insert(key.into(), Some(value.into()));
        self
    }
    fn env_remove(&mut self, key: &str) -> &mut Self {
        self.environment.insert(key.into(), None);
        self
    }
    fn output(&self) -> std::io::Result<Output> {
        if self.server.target == Target::Reference {
            let environment = self
                .environment
                .iter()
                .map(|(key, value)| (key.as_bytes(), value.as_deref().map(str::as_bytes)))
                .collect::<Vec<_>>();
            return Ok(self
                .server
                .reference
                .borrow()
                .as_ref()
                .unwrap()
                .output_with_environment(
                    &self
                        .args
                        .iter()
                        .map(|arg| arg.as_bytes())
                        .collect::<Vec<_>>(),
                    &[],
                    &environment,
                ));
        }
        let mut args = Vec::new();
        for arg in &self.args {
            let script=match arg.as_str() {
                "sleep 120"=>Some("import time; time.sleep(120)".to_owned()),
                "printf '%s\\n' ENV \"${PARITY_INHERITED-unset}\" \"${PARITY_MASK-unset}\" \"${PARITY_SECRET-unset}\" \"$PARITY_EXTRA\" \"$TERM\" \"$SHELL\" \"$PATH\"; exec /bin/sleep 120"=>Some("import os,time; print('ENV',*(os.environ.get(k,'unset') for k in ['PARITY_INHERITED','PARITY_MASK','PARITY_SECRET','PARITY_EXTRA','TERM','SHELL','PATH']),sep='\\n',flush=True); time.sleep(120)".into()),
                "printf 'IDENTITY\\n%s\\n%s\\n' \"$TERM_PROGRAM\" \"$TERM_PROGRAM_VERSION\"; exec /bin/sleep 120"=>Some("import os,time; print('IDENTITY',os.environ['TERM_PROGRAM'],os.environ['TERM_PROGRAM_VERSION'],sep='\\n',flush=True); time.sleep(120)".into()),
                "printf 'PATH\\n%s\\n' \"$PATH\"; exec /bin/sleep 120"=>Some("import os,time; print('PATH',os.environ.get('PATH',''),sep='\\n',flush=True); time.sleep(120)".into()),
                "/usr/bin/env"=>Some("import os; print('\\n'.join(k+'='+v for k,v in os.environ.items()),flush=True)".into()),
                _=>None,
            };
            if let Some(script) = script {
                args.extend([python().to_owned(), "-u".into(), "-c".into(), script]);
            } else {
                args.push(arg.clone());
            }
        }
        let mut command = Command::new(EXE);
        command
            .args(["-L", &self.server.label])
            .args(&args)
            .current_dir(&self.server.directory);
        for (key, value) in &self.environment {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        Ok(bounded(&mut command))
    }
}

#[test]
#[ignore = "requires pinned real C reference and native pinned ConPTY"]
fn startup_shell_and_editor_defaults_match_c() {
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
        (Some("SELF"), None, Some("vim"), "vim", "vi"),
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
        let mut tails = Vec::new();
        for target in [Target::Reference, Target::Native] {
            // SELF must be that implementation's executable, never an arbitrary
            // missing path which would fail to exercise recursive-shell rejection.
            let remote_identity = if shell == Some("SELF") && target == Target::Reference {
                let probe = RemoteReference::new();
                let identity: serde_json::Value =
                    serde_json::from_slice(&probe.identity()).unwrap();
                Some(identity["binary"].as_str().unwrap().to_owned())
            } else {
                None
            };
            let actual_shell = if shell == Some("SELF") {
                Some(remote_identity.as_deref().unwrap_or(EXE))
            } else {
                shell
            };
            let server = Server::new(target);
            let output = server.start(actual_shell, visual, editor);
            assert!(output.status.success(), "{target:?} {shell:?}: {output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            let (observed_shell, tail) = text.split_once('|').unwrap();
            assert_eq!(tail, format!("{expected_editor}|{keys}|{keys}\n"));
            if target == Target::Native {
                assert_eq!(observed_shell, native_shell());
            } else if shell == Some("/bin/sh") {
                assert_eq!(observed_shell, "/bin/sh");
            } else {
                assert!(observed_shell.starts_with('/'));
            }
            tails.push(tail.to_owned());
        }
        assert_eq!(tails[0], tails[1]);
    }
}

#[test]
#[ignore = "requires current C tmux with client environment format loops"]
fn client_environment_format_uses_request_environment() {
    let oracle = Target::Reference;
    let c = Server::new(oracle);
    let rust = Server::new(Target::Native);
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
    let mut observations = Vec::new();
    for program in [Target::Reference, Target::Native] {
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
                        format!(
                            "ENV\nglobal\nunset\nunset\nlast\ncustom-term\n{}\n/client/path\n",
                            if program == Target::Native {
                                native_shell()
                            } else {
                                "/bin/sh".into()
                            }
                        )
                        .as_bytes(),
                        "{program:?} {name}"
                    );
                    outputs.push(
                        capture
                            .split(|b| *b == b'\n')
                            .enumerate()
                            .filter(|(i, _)| *i != 6)
                            .flat_map(|(_, line)| line.iter().copied().chain([b'\n']))
                            .collect::<Vec<_>>(),
                    );
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
                expected
                    .map(
                        |value| if value == "/usr/bin:/bin" && program == Target::Native {
                            native_fallback_path().into_bytes()
                        } else {
                            value.as_bytes().to_vec()
                        }
                    )
                    .as_deref(),
                "{program:?} {flags} {override_value:?} {caller:?}"
            );
            outputs.push(if expected == Some("/usr/bin:/bin") {
                b"platform-default".to_vec()
            } else {
                path.unwrap_or_default().to_vec()
            });
        }
        observations.push(outputs);
    }
    assert_eq!(observations[0], observations[1]);
}

#[test]
#[ignore = "requires C tmux and real pane processes"]
fn implementation_identity_is_explicit_and_pane_overrides_take_precedence() {
    use std::time::{Duration, Instant};
    let oracle = Target::Reference;
    let c_version = RemoteReference::new().output(&[b"-V"], &[]);
    assert!(c_version.status.success());
    let c_version = String::from_utf8(c_version.stdout).unwrap();
    let c_version = c_version.trim().strip_prefix("tmux ").unwrap();
    for (program, identity, version) in [
        (oracle, "tmux", c_version),
        (Target::Native, "tmuxx", env!("CARGO_PKG_VERSION")),
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
    for program in [Target::Reference, Target::Native] {
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
