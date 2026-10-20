//! Windows fixture applications for the unchanged scheduling/command matrices.
//! C commands execute only on the pinned, leased reference host.
#[path = "../../../../conformance/remote_reference.rs"]
mod remote_reference;
use super::{Fixture, Response, command, fixture_shell, ok, powershell};
use remote_reference::{RemoteControl, RemoteReference};
use std::{
    path::PathBuf,
    process::{ExitStatus, Output},
    time::{Duration, Instant},
};

pub(super) fn job(value: &str) -> Option<String> {
    Some(match value {
        "/bin/zsh" => fixture_shell(),
        "/tmp" => std::env::temp_dir().to_str().unwrap().trim_end_matches('\\').into(),
        "printf raw" => powershell("[Console]::Write('raw')"),
        "printf 'a\\nb\\nlast'" => powershell("[Console]::Write(\"a`nb`nlast\")"),
        "printf 'a\\000hidden\\nb\\000hidden'" => powershell("[Console]::Write(\"a`0hidden`nb`0hidden\")"),
        "printf err >&2" => powershell("[Console]::Error.Write('err')"),
        "printf '%s:%s' '#{session_name}' '#{1}'" => "python -c \"import sys;sys.stdout.write(':'.join(sys.argv[1:]))\" \"#{session_name}\" \"#{1}\"".into(),
        "printf ignored-flag" => powershell("[Console]::Write('ignored-flag')"),
        "printf leading-space" => powershell("[Console]::Write('leading-space')"),
        "printf hex-float" => powershell("[Console]::Write('hex-float')"),
        "printf empty-delay" => powershell("[Console]::Write('empty-delay')"),
        "printf '%s|%s|%s' \"$0\" \"$TERM\" \"$COLORTERM\"" => powershell("[Console]::Write((Get-Process -Id (Get-CimInstance Win32_Process -Filter ('ProcessId='+$PID)).ParentProcessId).Path+'|'+$env:TERM+'|'+$env:COLORTERM)"),
        "pwd" => powershell("[Console]::Write($env:PWD)"),
        _ => return None,
    })
}

fn replace(input: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    assert!(!from.is_empty());
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        if input[offset..].starts_with(from) {
            output.extend_from_slice(to);
            offset += from.len();
        } else {
            output.push(input[offset]);
            offset += 1;
        }
    }
    output
}
fn code(value: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(value as u32)
}

pub(super) struct Oracle {
    remote: RemoteReference,
    local: PathBuf,
    directory: String,
    reference_home: String,
}
impl Oracle {
    pub(super) fn new(fixture: &Fixture) -> Self {
        let remote = RemoteReference::new();
        let directory = String::from_utf8(remote.directory()).unwrap();
        // Capture the reference's actual login home before the matrix installs
        // hooks, changes environment or starts pane applications.
        let environment = remote.output(&[b"show-environment", b"-g", b"HOME"], &[]);
        assert!(environment.status.success(), "{environment:?}");
        let home = String::from_utf8(environment.stdout).unwrap();
        let reference_home = home
            .strip_prefix("HOME=")
            .unwrap()
            .trim_end_matches('\n')
            .into();
        Self {
            remote,
            local: fixture.dir.clone(),
            directory,
            reference_home,
        }
    }
    pub(super) fn prepare(&self) {
        for item in std::fs::read_dir(&self.local).unwrap() {
            let path = item.unwrap().path();
            if path.is_file() {
                let bytes = std::fs::read(&path).unwrap();
                let reference = replace(&bytes, b"if-shell 'exit /b 0'", b"if-shell true");
                self.remote.put(
                    path.file_name().unwrap().to_str().unwrap().as_bytes(),
                    &reference,
                );
                let native = replace(&reference, b"if-shell true", b"if-shell 'exit /b 0'");
                if native != bytes {
                    std::fs::write(path, native).unwrap();
                }
            }
        }
    }
    fn arguments(&self, args: &[&str]) -> Vec<Vec<u8>> {
        let mut result = args
            .iter()
            .map(|value| {
                // Use the installed alternate Unix shell on the reference host;
                // Windows changes cmd.exe to explicit PowerShell before job checks.
                if *value == "/bin/zsh" {
                    b"/bin/bash".to_vec()
                } else {
                    value.as_bytes().to_vec()
                }
            })
            .collect::<Vec<_>>();
        if args.first() == Some(&"new-session") {
            result.splice(1..1, [b"-c".to_vec(), self.directory.as_bytes().to_vec()]);
        }
        result
    }
    pub(super) fn start(&self, args: &[&str]) -> RemoteControl {
        self.prepare();
        let args = self.arguments(args);
        self.remote
            .command_open_args(&args.iter().map(Vec::as_slice).collect::<Vec<_>>())
    }
    pub(super) fn finish(&self, child: RemoteControl) -> Output {
        assert!(child.finish_input().status.success());
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            let output = child.read();
            assert!(output.status.success(), "{output:?}");
            stderr.extend_from_slice(&output.stderr);
            if output.stdout.is_empty() {
                break;
            }
            stdout.extend_from_slice(&output.stdout);
            assert!(Instant::now() < deadline, "C command deadline");
        }
        let status = loop {
            if let Some(status) = child.exited() {
                break status;
            }
            assert!(Instant::now() < deadline, "C command exit deadline");
            std::thread::sleep(Duration::from_millis(2));
        };
        Output {
            status: code(status),
            stdout: self.canonical(&stdout),
            stderr: self.canonical(&stderr),
        }
    }
    pub(super) fn run(&self, args: &[&str]) -> Output {
        // Separate stdout/stderr are part of the command contract. One-shot
        // commands use the exact-byte API; pending event waits use start/finish.
        self.prepare();
        let args = self.arguments(args);
        let mut output = self
            .remote
            .output(&args.iter().map(Vec::as_slice).collect::<Vec<_>>(), &[]);
        output.stdout = self.canonical(&output.stdout);
        output.stderr = self.canonical(&output.stderr);
        output
    }
    fn canonical(&self, bytes: &[u8]) -> Vec<u8> {
        // Normalize only independently known native/reference platform values.
        // Every command byte, status, line boundary and ordering remains exact.
        let mut result = bytes.to_vec();
        let local = self.local.to_str().unwrap();
        for item in std::fs::read_dir(&self.local).unwrap() {
            let path = item.unwrap().path();
            if path.is_file() {
                let leaf = path.file_name().unwrap().to_str().unwrap();
                let canonical = format!("<fixture>/{leaf}");
                for native in [path.canonicalize().unwrap(), path.clone()] {
                    result = replace(
                        &result,
                        native.to_str().unwrap().as_bytes(),
                        canonical.as_bytes(),
                    );
                }
                result = replace(
                    &result,
                    format!("{}/{leaf}", self.directory).as_bytes(),
                    canonical.as_bytes(),
                );
            }
        }
        for root in [
            // NT process parameters retain a trailing separator for cwd.
            // This is the same independently known private directory.
            format!("{local}\\"),
            local.to_string(),
            local.replace('\\', "/"),
            self.directory.clone(),
        ] {
            result = replace(&result, root.as_bytes(), b"<fixture>");
        }
        let temporary = std::env::temp_dir();
        result = replace(
            &result,
            temporary
                .to_str()
                .unwrap()
                .trim_end_matches('\\')
                .as_bytes(),
            b"<tmp>",
        );
        result = replace(&result, b"/tmp", b"<tmp>");
        let home = std::env::var("USERPROFILE").unwrap();
        for home in [
            home.clone(),
            home.replace('\\', "/"),
            self.reference_home.clone(),
        ] {
            result = replace(&result, home.as_bytes(), b"<home>");
        }
        let shell = fixture_shell();
        result = replace(&result, shell.as_bytes(), b"/bin/sh");
        result
    }
    pub(super) fn normalize(&self, mut response: Response) -> Response {
        response.output = self.canonical(&response.output);
        response.error = response
            .error
            .map(|error| String::from_utf8(self.canonical(error.as_bytes())).unwrap());
        response
    }
}

pub(super) fn compare_system_shell(args: &[&str], native: &Response, reference: &Output) -> bool {
    if args != ["run", "printf '%s|%s|%s' \"$0\" \"$TERM\" \"$COLORTERM\""] {
        return false;
    }
    assert_eq!(native.error, None);
    assert_eq!(native.status, Some(0));
    assert!(reference.status.success());
    assert!(reference.stderr.is_empty());
    let text = std::str::from_utf8(&native.output).unwrap();
    let path = text
        .strip_suffix("|vt220|truecolor\n")
        .expect("exact native job environment and line framing");
    let expected = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    assert_eq!(
        PathBuf::from(path).canonicalize().unwrap(),
        expected.canonicalize().unwrap(),
        "actual parent executable is the system cmd.exe despite changed default-shell"
    );
    // C executes /bin/sh with its basename as argv[0] on this Linux reference.
    // Windows observes the actual parent image path instead of inventing $0.
    assert_eq!(reference.stdout, b"sh|vt220|truecolor\n");
    true
}

pub(super) fn named_user_tilde() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = super::reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    let name = String::from_utf8(ok(&client, &["display", "-p", "#{user}"]).output).unwrap();
    let name = name.trim_end_matches('\n');
    let home = std::env::var("USERPROFILE").unwrap();
    assert!(!name.is_empty() && !name.contains(['\\', '/', '\n']));
    let source = "display -p ~\ndisplay -p ~/bin\ndisplay -p ~root\ndisplay -p ~root/bin\ndisplay -p '~root'\ndisplay -p \"~root\"\ndisplay -p mid~root\ndisplay -p prefix\"~root\"\ndisplay -p \"\"~root\n";
    for configured in [Some("/tmuxx-home with spaces"), Some(""), None] {
        let args = if let Some(value) = configured {
            vec!["setenv", "-g", "HOME", value]
        } else {
            vec!["setenv", "-gu", "HOME"]
        };
        super::assert_reference(&client, &oracle, &args);
        std::fs::write(
            fixture.dir.join("tilde.conf"),
            source.replace("~root", &format!("~{name}")),
        )
        .unwrap();
        oracle.remote.put(b"tilde.conf", source.as_bytes());
        let native = ok(&client, &["source-file", "tilde.conf"]);
        let c = oracle.remote.output(&[b"source-file", b"tilde.conf"], &[]);
        assert!(c.status.success(), "{c:?}");
        let current = configured
            .filter(|value| !value.is_empty())
            .unwrap_or(&home);
        let expected = format!(
            "{current}\n{current}/bin\n{home}\n{home}/bin\n~{name}\n{home}\nmid~{name}\nprefix{home}\n{home}\n"
        );
        assert_eq!(native.output, expected.as_bytes());
        let c_current = configured
            .filter(|value| !value.is_empty())
            .unwrap_or(&oracle.reference_home);
        assert_eq!(c.stdout, format!("{c_current}\n{c_current}/bin\n/root\n/root/bin\n~root\n/root\nmid~root\nprefix/root\n/root\n").as_bytes());
        let alias = format!("account-home=display-message -p ~{name}/bin");
        ok(&client, &["set", "-s", "command-alias[50]", &alias]);
        assert_eq!(
            ok(&client, &["account-home"]).output,
            format!("{home}/bin\n").as_bytes()
        );
        let c = oracle.remote.output(
            &[
                b"set",
                b"-s",
                b"command-alias[50]",
                b"account-home=display-message -p ~root/bin",
            ],
            &[],
        );
        assert!(c.status.success(), "{c:?}");
        let c = oracle.remote.output(&[b"account-home"], &[]);
        assert!(c.status.success(), "{c:?}");
        assert_eq!(c.stdout, b"/root/bin\n");
    }
    for source in [
        "HOME='/prepared value'\ndisplay -p ~\n".to_string(),
        "display -p ~tmuxx_no_such_account_42\n".into(),
        format!("display -p ~{}\n", "a".repeat(1023)),
    ] {
        std::fs::write(fixture.dir.join("tilde.conf"), &source).unwrap();
        super::assert_reference(&client, &oracle, &["source-file", "tilde.conf"]);
        let native = command(&client, &["source-file", "tilde.conf"]);
        if source.starts_with("HOME=") {
            assert_eq!(native.output, b"/prepared value\n");
        } else {
            assert_eq!(native.status, Some(1));
            assert!(
                native
                    .output
                    .ends_with(if source.contains("tmuxx_no_such") {
                        b"syntax error\n"
                    } else {
                        b"user name is too long\n"
                    })
            );
        }
    }
}

pub(super) fn job_stdin_and_termination() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = super::reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    let native = powershell(
        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class NativeInput { [DllImport(\"kernel32.dll\")] public static extern IntPtr GetStdHandle(int n); [DllImport(\"kernel32.dll\")] public static extern uint GetFileType(IntPtr h); }'; [Console]::Write([NativeInput]::GetFileType([NativeInput]::GetStdHandle(-10)).ToString()+':'+$(if ([Console]::In.Read() -eq -1) {'eof'} else {'input'}))",
    );
    // Windows jobs receive a pipe, not a terminal/console or NUL. Its writer
    // closes before execution, delivering EOF without retaining owner handles.
    assert_eq!(ok(&client, &["run-shell", &native]).output, b"3:eof\n");
    let c = oracle.run(&["run-shell", "if test -S /dev/fd/0; then printf socket:; else printf other:; fi; if read line; then printf input; else printf eof; fi"]);
    assert!(c.status.success());
    assert_eq!(c.stdout, b"socket:eof\n");
    let terminate = powershell("[Diagnostics.Process]::GetCurrentProcess().Kill()");
    let result = ok(
        &client,
        &["run-shell", &terminate, ";", "display", "-p", "after"],
    );
    assert_eq!(result.status, Some(-1));
    assert_eq!(
        result.output,
        format!("'{terminate}' returned -1\nafter\n").as_bytes()
    );
    let c = oracle.run(&["run-shell", "kill -TERM $$", ";", "display", "-p", "after"]);
    assert_eq!(c.status.code(), Some(143));
    assert_eq!(
        c.stdout,
        b"'kill -TERM $$' terminated by signal 15\nafter\n"
    );
    assert_eq!(ok(&client, &["run-shell", "true"]).status, Some(0));
}
