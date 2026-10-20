#![cfg(any(unix, windows))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tmuxx_protocol::native::{Request, Response, ServerMessage};
use tmuxx_runtime::{ClientContext, LocalClient, Options, ServerHandle};
#[cfg(windows)]
#[path = "scheduler/windows.rs"]
mod windows;
#[cfg(windows)]
use windows::Oracle;

fn fixture_shell() -> String {
    #[cfg(unix)]
    return "/bin/sh".into();
    #[cfg(windows)]
    return PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
        .to_str()
        .unwrap()
        .to_owned();
}

#[cfg(windows)]
fn powershell(source: &str) -> String {
    // The scheduler deliberately uses system cmd.exe; invoke the explicit
    // PowerShell fixture without changing the production shell contract.
    const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = source
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (shift, present) in [
            (18, true),
            (12, true),
            (6, chunk.len() > 1),
            (0, chunk.len() > 2),
        ] {
            encoded.push(if present {
                DIGITS[((bits >> shift) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    format!(
        "\"{}\" -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}",
        fixture_shell()
    )
}

fn fixture_argv(argv: &[&str]) -> Vec<Vec<u8>> {
    let values = argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    #[cfg(windows)]
    let mut values = values;
    #[cfg(windows)]
    {
        // Equivalent private fixture programs run in the actual Windows shell;
        // the commands under test still traverse the production job scheduler.
        for value in &mut values {
            if let Some(replacement) = windows::job(value) {
                *value = replacement;
                continue;
            }
            if value == "printf '%s' '#{1}'" {
                // Keep the placeholder visible to production job formatting.
                *value = "python -c \"import sys;sys.stdout.write(sys.argv[1])\" \"#{1}\"".into();
                continue;
            }
            let replacement = match value.as_str() {
                "true" => {
                    *value = "exit /b 0".into();
                    continue;
                }
                "false" => {
                    *value = "exit /b 1".into();
                    continue;
                }
                "printf ready > ready; while [ ! -e release ]; do sleep 0.01; done; printf JOB" => {
                    "[IO.File]::WriteAllText('ready','ready'); while (!(Test-Path -LiteralPath 'release')) { Start-Sleep -Milliseconds 10 }; [Console]::Write('JOB')"
                }
                "printf hidden >&2" => "[Console]::Error.Write('hidden')",
                "printf stderr >&2" => "[Console]::Error.Write('stderr')",
                "touch forbidden" => "[IO.File]::WriteAllText('forbidden','')",
                "sleep 0.02; printf done > background" => {
                    "Start-Sleep -Milliseconds 20; [IO.File]::WriteAllText('background','done')"
                }
                "sleep 30 & echo $! > descendant; exit 0" => {
                    "python -c \"import pathlib,subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); pathlib.Path('descendant.tmp').write_text(str(child.pid)); pathlib.Path('descendant.tmp').replace('descendant')\"; exit 0"
                }
                _ => continue,
            };
            *value = powershell(replacement);
        }
        if values.len() >= 2 && values[values.len() - 2..] == ["sleep", "30"] {
            values.truncate(values.len() - 2);
            values.extend([
                std::env::var("TMUXX_REMOTE_PYTHON").unwrap_or_else(|_| "python".into()),
                "-c".into(),
                "import time; time.sleep(30)".into(),
            ]);
        }
    }
    values.iter().map(|arg| arg.as_bytes().to_vec()).collect()
}

#[cfg(windows)]
fn windows_process(pid: u32) -> std::os::windows::io::OwnedHandle {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!handle.is_null());
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

#[cfg(windows)]
fn assert_windows_dead(process: &std::os::windows::io::OwnedHandle) {
    use std::os::windows::io::AsRawHandle;
    assert_eq!(
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                process.as_raw_handle(),
                3000,
            )
        },
        0,
        "shell descendant survived owner shutdown"
    );
}

struct Fixture {
    host: Option<ServerHandle>,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tmuxx-schedule-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        let endpoint = dir.join("native");
        #[cfg(windows)]
        let endpoint =
            tmuxx_runtime::windows::endpoint(dir.file_name().unwrap().to_str().unwrap()).unwrap();
        let host = tmuxx_runtime::start(Options::new(endpoint)).unwrap();
        let client = host.connect().unwrap();
        ok(
            &client,
            &["set-option", "-g", "default-shell", &fixture_shell()],
        );
        Self {
            host: Some(host),
            dir,
        }
    }
    fn client(&self) -> LocalClient {
        self.host
            .as_ref()
            .unwrap()
            .connect_with_context(ClientContext {
                cwd: Some(self.dir.to_str().unwrap().to_owned()),
                ..ClientContext::default()
            })
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.host.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn send(client: &LocalClient, id: u64, argv: &[&str]) {
    client
        .send(Request {
            request_id: id,
            argv: fixture_argv(argv),
            stdin: None,
            subscribe: false,
            control: false,
        })
        .unwrap();
}
fn reply(client: &LocalClient) -> Response {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.try_recv() {
            Ok(ServerMessage::Reply(response)) => return response,
            Ok(message) => panic!("unexpected response: {message:?}"),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("server disconnected"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for command reply"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn command(client: &LocalClient, argv: &[&str]) -> Response {
    send(client, 1, argv);
    reply(client)
}
fn ok(client: &LocalClient, argv: &[&str]) -> Response {
    let response = command(client, argv);
    assert_eq!(response.error, None, "{argv:?}");
    response
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "job did not create {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn wait_channels_and_locks_pause_only_their_client_and_preserve_fifo() {
    let fixture = Fixture::new();
    let first = fixture.client();
    let second = fixture.client();
    let admin = fixture.client();
    send(
        &first,
        10,
        &["wait-for", "ready", ";", "display-message", "-p", "first"],
    );
    send(&first, 11, &["display-message", "-p", "queued"]);
    send(
        &second,
        20,
        &["wait-for", "ready", ";", "display-message", "-p", "second"],
    );
    ok(&admin, &["display-message", "-p", "responsive"]);
    assert!(first.try_recv().is_err());
    assert!(second.try_recv().is_err());
    ok(&admin, &["wait-for", "-S", "ready"]);
    assert_eq!(reply(&first).output, b"first\n");
    assert_eq!(reply(&first).request_id, 11);
    assert_eq!(reply(&second).output, b"second\n");

    ok(&admin, &["wait-for", "-L", "mutex"]);
    send(
        &first,
        30,
        &[
            "wait-for",
            "-L",
            "mutex",
            ";",
            "display-message",
            "-p",
            "one",
        ],
    );
    send(
        &second,
        40,
        &[
            "wait-for",
            "-L",
            "mutex",
            ";",
            "display-message",
            "-p",
            "two",
        ],
    );
    ok(&admin, &["wait-for", "-U", "mutex"]);
    assert_eq!(reply(&first).output, b"one\n");
    assert!(second.try_recv().is_err());
    ok(&first, &["wait-for", "-U", "mutex"]);
    assert_eq!(reply(&second).output, b"two\n");
    ok(&second, &["wait-for", "-U", "mutex"]);
    assert!(
        command(&admin, &["wait-for", "-U", "mutex"])
            .error
            .unwrap()
            .contains("not locked")
    );

    ok(
        &admin,
        &[
            "wait-for", "-L", "combined", ";", "wait-for", "-S", "combined", ";", "wait-for",
            "combined", ";", "wait-for", "combined", ";", "wait-for", "-U", "combined",
        ],
    );
    ok(
        &admin,
        &["wait-for", "-S", "saved", ";", "wait-for", "saved"],
    );
    ok(
        &admin,
        &["wait-for", "-S", "double", ";", "wait-for", "-S", "double"],
    );
    send(&first, 50, &["wait-for", "double"]);
    ok(&admin, &["display-message", "-p", "barrier"]);
    assert!(first.try_recv().is_err());
    drop(first);
    ok(&admin, &["wait-for", "-S", "double"]);
}

#[test]
fn foreground_jobs_keep_owner_responsive_and_status_does_not_cancel_group() {
    let fixture = Fixture::new();
    let waiting = fixture.client();
    let admin = fixture.client();
    send(
        &waiting,
        10,
        &[
            "run-shell",
            "printf ready > ready; while [ ! -e release ]; do sleep 0.01; done; printf JOB",
            ";",
            "display-message",
            "-p",
            "after",
        ],
    );
    send(&waiting, 11, &["display-message", "-p", "queued"]);
    wait_file(&fixture.dir.join("ready"));
    assert_eq!(
        ok(&admin, &["display-message", "-p", "responsive"]).output,
        b"responsive\n"
    );
    assert!(waiting.try_recv().is_err());
    std::fs::write(fixture.dir.join("release"), b"").unwrap();
    let done = reply(&waiting);
    assert_eq!(done.request_id, 10);
    assert_eq!(done.output, b"JOB\nafter\n");
    assert_eq!(done.status, Some(0));
    assert_eq!(reply(&waiting).request_id, 11);
    let failed = ok(
        &admin,
        &[
            "run-shell",
            "exit 7",
            ";",
            "display-message",
            "-p",
            "still-runs",
        ],
    );
    assert_eq!(failed.status, Some(7));
    assert_eq!(failed.output, b"'exit 7' returned 7\nstill-runs\n");
    assert_eq!(
        ok(&admin, &["run-shell", "exit 7", ";", "run-shell", "true"]).status,
        Some(0)
    );
    let failed = command(
        &admin,
        &[
            "run-shell",
            "true",
            ";",
            "not-a-command",
            ";",
            "display-message",
            "-p",
            "must-not-run",
        ],
    );
    assert!(failed.error.is_some());
    assert!(!failed.output.ends_with(b"must-not-run\n"));
    ok(&admin, &["display-message", "-p", "recovered"]);
}

#[test]
fn conditional_and_delayed_commands_share_queue_context_and_explicit_errors() {
    let fixture = Fixture::new();
    let client = fixture.client();
    assert_eq!(
        ok(
            &client,
            &[
                "if-shell",
                "true",
                "display-message -p yes",
                "display-message -p no",
                ";",
                "display-message",
                "-p",
                "after"
            ]
        )
        .output,
        b"yes\nafter\n"
    );
    assert_eq!(
        ok(
            &client,
            &[
                "if-shell",
                "false",
                "display-message -p yes",
                "display-message -p no"
            ]
        )
        .output,
        b"no\n"
    );
    assert_eq!(
        ok(
            &client,
            &[
                "if-shell",
                "-F",
                "00",
                "display-message -p yes",
                "display-message -p no"
            ]
        )
        .output,
        b"no\n"
    );
    assert_eq!(
        ok(
            &client,
            &[
                "run-shell",
                "-C",
                "-d",
                "0.01",
                "display-message -p delayed",
                ";",
                "display-message",
                "-p",
                "after"
            ]
        )
        .output,
        b"delayed\nafter\n"
    );
    assert_eq!(
        ok(&client, &["run-shell", "printf '%s' '#{1}'", "argument"]).output,
        b"argument\n"
    );
    assert_eq!(ok(&client, &["run-shell", "printf hidden >&2"]).output, b"");
    assert_eq!(
        ok(&client, &["run-shell", "-E", "printf stderr >&2"]).output,
        b"stderr\n"
    );
    assert_eq!(
        ok(
            &client,
            &["run-shell", "-C", "display-message -p extra", "ignored"]
        )
        .output,
        b"extra\n"
    );
    assert!(
        command(&client, &["wait-for", "-E", "unsupported-event"])
            .error
            .is_some()
    );
    assert!(
        command(&client, &["run-shell", "-t", "%0", "touch forbidden"])
            .error
            .is_some()
    );
    assert!(!fixture.dir.join("forbidden").exists());
}

#[test]
fn background_jobs_and_disconnected_waiters_do_not_block_remaining_clients() {
    let fixture = Fixture::new();
    let first = fixture.client();
    let admin = fixture.client();
    ok(
        &first,
        &["run-shell", "-b", "sleep 0.02; printf done > background"],
    );
    drop(first);
    wait_file(&fixture.dir.join("background"));
    ok(
        &admin,
        &["if-shell", "-b", "true", "set-environment -g SCHEDULED yes"],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if command(&admin, &["show-environment", "-g", "SCHEDULED"]).output == b"SCHEDULED=yes\n" {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let lost = fixture.client();
    send(
        &lost,
        99,
        &[
            "wait-for",
            "-L",
            "lost",
            ";",
            "wait-for",
            "-L",
            "lost",
            ";",
            "set-environment",
            "-g",
            "REVIVED",
            "bad",
        ],
    );
    ok(&admin, &["display-message", "-p", "barrier"]);
    drop(lost);
    ok(&admin, &["wait-for", "-U", "lost"]);
    assert!(
        command(&admin, &["show-environment", "-g", "REVIVED"])
            .error
            .is_some()
    );
}

#[test]
fn command_only_job_target_formats_survive_preparation() {
    let fixture = Fixture::new();
    let client = fixture.client();
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    assert_eq!(
        ok(
            &client,
            &[
                "run",
                "-Ct",
                "main:0.0",
                "display -p '#{session_name}:#{pane_id}:#{command}'"
            ]
        )
        .output,
        b"main:%0:run-shell\n"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires pinned remote C oracle"]
fn time_literals_and_incomplete_directives_match_c() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    for format in [
        "main:%0:run-shell",
        "%1|%42|%Q|%q|%N|%E|%O",
        "%",
        "界%0/😀%q",
        "year:%Y|%q|percent:%%",
    ] {
        assert_reference(&client, &oracle, &["display", "-p", format]);
    }
}

#[test]
fn shutdown_reaps_shell_descendants_that_keep_stdout_open() {
    let mut fixture = Fixture::new();
    let client = fixture.client();
    send(
        &client,
        1,
        &["run-shell", "sleep 30 & echo $! > descendant; exit 0"],
    );
    let path = fixture.dir.join("descendant");
    wait_file(&path);
    let pid: i32 = std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    #[cfg(windows)]
    let process = windows_process(pid as u32);
    let start = Instant::now();
    fixture.host.take().unwrap().shutdown().unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    #[cfg(windows)]
    assert_windows_dead(&process);
    #[cfg(unix)]
    let deadline = Instant::now() + Duration::from_secs(3);
    #[cfg(unix)]
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "shell descendant survived owner shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn relative_source_and_hidden_assignments_affect_jobs_but_do_not_export_secrets() {
    let fixture = Fixture::new();
    let client = fixture.client();
    #[cfg(unix)]
    std::fs::write(fixture.dir.join("config"), b"%hidden SECRET=value\nVISIBLE=present\nrun-shell 'printf \"%s:%s\" \"$VISIBLE\" \"${SECRET-unset}\"'\n").unwrap();
    #[cfg(windows)]
    std::fs::write(fixture.dir.join("config"), format!("%hidden SECRET=value\nVISIBLE=present\nrun-shell '{}'\n",
        powershell(r#"[Console]::Write($env:VISIBLE+":"+$(if (Test-Path Env:SECRET) {$env:SECRET} else {"unset"}))"#))).unwrap();
    assert_eq!(
        ok(&client, &["source-file", "config"]).output,
        b"present:unset\n"
    );
    assert_eq!(
        ok(&client, &["show-environment", "-gh", "SECRET"]).output,
        b"SECRET=value\n"
    );
}

#[cfg(unix)]
use std::process::{Child, Command, Output, Stdio};
#[cfg(unix)]
struct Oracle {
    program: PathBuf,
    socket: PathBuf,
}
#[cfg(unix)]
impl Oracle {
    fn new(fixture: &Fixture) -> Self {
        Self {
            program: std::env::var_os("TMUXX_C_TMUX")
                .expect("set TMUXX_C_TMUX")
                .into(),
            socket: fixture.dir.join("oracle"),
        }
    }
    fn start(&self, args: &[&str]) -> Child {
        Command::new(&self.program)
            .args(["-S"])
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .env("SHELL", "/bin/sh")
            .current_dir(self.socket.parent().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        self.finish(self.start(args))
    }
    fn finish(&self, mut child: Child) -> Output {
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("C oracle timed out");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        child.wait_with_output().unwrap()
    }
}
#[cfg(unix)]
impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}

#[test]
#[ignore = "requires TMUXX_C_TMUX pointing to a separately built C oracle"]
fn shell_condition_wait_and_exit_status_match_c_reference() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = Oracle::new(&fixture);
    let created = oracle.run(&[
        "new-session",
        "-d",
        "-s",
        "schedule",
        "sleep",
        "30",
        ";",
        "set-option",
        "-g",
        "default-shell",
        "/bin/sh",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    ok(
        &client,
        &["new-session", "-d", "-s", "schedule", "sleep", "30"],
    );
    for args in [
        vec![
            "run-shell",
            "printf raw",
            ";",
            "display-message",
            "-p",
            "after",
        ],
        vec!["run-shell", "exit 7", ";", "display-message", "-p", "after"],
        vec!["run-shell", "exit 7", ";", "run-shell", "true"],
        vec!["run-shell", "printf 'a\\nb\\nlast'"],
        vec!["run-shell", "printf 'a\\000hidden\\nb\\000hidden'"],
        vec!["run-shell", "-E", "printf err >&2"],
        vec!["run-shell", "printf hidden >&2"],
        vec![
            "run-shell",
            "printf '%s:%s' '#{session_name}' '#{1}'",
            "argument",
        ],
        vec![
            "run-shell",
            "-C",
            "-d",
            "0.01",
            "display-message -p delayed",
            ";",
            "display-message",
            "-p",
            "after",
        ],
        vec![
            "if-shell",
            "true",
            "display-message -p yes",
            "display-message -p no",
        ],
        vec![
            "if-shell",
            "false",
            "display-message -p yes",
            "display-message -p no",
        ],
        vec![
            "if-shell",
            "-F",
            "00",
            "display-message -p yes",
            "display-message -p no",
        ],
        vec![
            "if-shell",
            "-F",
            "abc",
            "display-message -p yes",
            "display-message -p no",
        ],
        vec![
            "wait-for",
            "-S",
            "ready",
            ";",
            "wait-for",
            "ready",
            ";",
            "display-message",
            "-p",
            "after",
        ],
        vec![
            "wait-for", "-L", "mutex", ";", "wait-for", "-S", "mutex", ";", "wait-for", "mutex",
            ";", "wait-for", "mutex", ";", "wait-for", "-U", "mutex",
        ],
    ] {
        let c = oracle.run(&args);
        let rust = ok(&client, &args);
        assert_eq!(
            rust.output,
            c.stdout,
            "C output differs for {args:?}; C stderr {}",
            String::from_utf8_lossy(&c.stderr)
        );
        assert_eq!(
            rust.status.unwrap_or(0),
            c.status.code().unwrap(),
            "C exit status differs for {args:?}"
        );
    }
}

#[test]
fn event_wait_filters_listing_forced_wake_and_custom_events_keep_owner_responsive() {
    let fixture = Fixture::new();
    let waiting = fixture.client();
    let admin = fixture.client();
    send(
        &waiting,
        10,
        &[
            "wait-for",
            "-E",
            "-F",
            "#{==:#{session},$1}",
            "session-created",
            ";",
            "display-message",
            "-p",
            "matched",
        ],
    );
    assert!(
        !ok(&admin, &["wait-for", "-El", "session-created"])
            .output
            .is_empty()
    );
    ok(
        &admin,
        &["new-session", "-d", "-s", "ignored", "sleep", "30"],
    );
    ok(&admin, &["display-message", "-p", "barrier"]);
    assert!(waiting.try_recv().is_err());
    ok(
        &admin,
        &["new-session", "-d", "-s", "accepted", "sleep", "30"],
    );
    assert_eq!(reply(&waiting).output, b"matched\n");
    send(
        &waiting,
        11,
        &[
            "wait-for",
            "-E",
            "@custom",
            ";",
            "display-message",
            "-p",
            "custom",
        ],
    );
    ok(&admin, &["set-hook", "-E", "@custom"]);
    assert_eq!(reply(&waiting).output, b"custom\n");
    send(&waiting, 12, &["wait-for", "-E", "@forced"]);
    let who = String::from_utf8(ok(&admin, &["wait-for", "-El", "@forced"]).output).unwrap();
    ok(&admin, &["wait-for", "-Ew", who.trim(), "@forced"]);
    assert_eq!(reply(&waiting).request_id, 12);
    assert!(
        command(&admin, &["wait-for", "-Ew", who.trim(), "@forced"])
            .error
            .is_some()
    );
    send(&waiting, 13, &["wait-for", "ordinary"]);
    let who = String::from_utf8(ok(&admin, &["wait-for", "-l", "ordinary"]).output).unwrap();
    ok(&admin, &["wait-for", "-w", who.trim(), "ordinary"]);
    assert_eq!(reply(&waiting).request_id, 13);
}

#[test]
fn source_globs_dry_run_formats_aliases_and_shell_validation_are_effective() {
    let fixture = Fixture::new();
    let client = fixture.client();
    std::fs::write(
        fixture.dir.join("dry.conf"),
        b"PARSED=present\nset-option -g @dry-ran wrong\n",
    )
    .unwrap();
    ok(&client, &["source-file", "-n", "dry.conf"]);
    assert!(
        command(&client, &["show-environment", "-g", "PARSED"])
            .error
            .is_some()
    );
    assert_eq!(
        ok(&client, &["show-options", "-gqv", "@dry-ran"]).output,
        b""
    );
    std::fs::write(fixture.dir.join("00.conf"), b"set-option -g @glob zero\n").unwrap();
    std::fs::write(fixture.dir.join("01.conf"), b"set-option -ga @glob one\n").unwrap();
    ok(&client, &["set-option", "-g", "@source", "0*.conf"]);
    ok(&client, &["source-file", "-F", "#{@source}"]);
    assert_eq!(
        ok(&client, &["show-options", "-gqv", "@glob"]).output,
        b"zeroone\n"
    );
    ok(&client, &["source-file", "-q", "missing*.conf"]);
    ok(
        &client,
        &[
            "set-option",
            "-s",
            "command-alias[9]",
            "say=display-message -p",
        ],
    );
    assert_eq!(
        ok(&client, &["say", "literal ; value"]).output,
        b"literal ; value\n"
    );
    std::fs::write(fixture.dir.join("alias.conf"), b"say prepared\n").unwrap();
    assert_eq!(
        ok(&client, &["source-file", "alias.conf"]).output,
        b"prepared\n"
    );
    for bad in ["/tmuxx/does-not-exist", "relative", ""] {
        assert!(
            command(&client, &["set-option", "-g", "default-shell", bad])
                .error
                .unwrap()
                .starts_with("not a suitable shell:")
        );
        assert_eq!(
            ok(&client, &["show-options", "-gqv", "default-shell"]).output,
            format!("{}\n", fixture_shell()).as_bytes()
        );
    }
}

fn reference(fixture: &Fixture) -> Oracle {
    let oracle = Oracle::new(fixture);
    assert!(
        oracle
            .run(&["new-session", "-d", "-s", "main", "sleep", "30"])
            .status
            .success()
    );
    oracle
}
fn assert_reference(client: &LocalClient, oracle: &Oracle, args: &[&str]) {
    #[cfg(windows)]
    oracle.prepare();
    let rust = command(client, args);
    let c = oracle.run(args);
    #[cfg(windows)]
    if windows::compare_system_shell(args, &rust, &c) {
        return;
    }
    #[cfg(windows)]
    let rust = oracle.normalize(rust);
    assert_eq!(
        rust.output,
        c.stdout,
        "stdout for {args:?}; C stderr: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert_eq!(
        if rust.error.is_some() {
            1
        } else {
            rust.status.unwrap_or(0)
        },
        c.status.code().unwrap(),
        "status for {args:?}; Rust error {:?}; C stderr: {}",
        rust.error,
        String::from_utf8_lossy(&c.stderr)
    );
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn named_user_tilde_and_empty_home_fallback_match_c() {
    #[cfg(windows)]
    return windows::named_user_tilde();
    #[cfg(unix)]
    {
        let fixture = Fixture::new();
        let client = fixture.client();
        let oracle = reference(&fixture);
        ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
        let path = fixture.dir.join("tilde.conf");
        for home in [Some("/tmuxx-home with spaces"), Some(""), None] {
            let args = if let Some(home) = home {
                vec!["set-environment", "-g", "HOME", home]
            } else {
                vec!["set-environment", "-gu", "HOME"]
            };
            assert_reference(&client, &oracle, &args);
            std::fs::write(&path,b"display -p ~\ndisplay -p ~/bin\ndisplay -p ~root\ndisplay -p ~root/bin\ndisplay -p '~root'\ndisplay -p \"~root\"\ndisplay -p mid~root\ndisplay -p prefix\"~root\"\ndisplay -p \"\"~root\n").unwrap();
            assert_reference(&client, &oracle, &["source-file", "tilde.conf"]);
            assert_reference(
                &client,
                &oracle,
                &[
                    "set-option",
                    "-s",
                    "command-alias[50]",
                    "account-home=display-message -p ~root/bin",
                ],
            );
            assert_reference(&client, &oracle, &["account-home"]);
        }
        std::fs::write(&path, b"HOME='/prepared value'\ndisplay -p ~\n").unwrap();
        assert_reference(&client, &oracle, &["source-file", "tilde.conf"]);
        for source in [
            "display -p ~tmuxx_no_such_account_42\n".to_string(),
            format!("display -p ~{}\n", "a".repeat(1023)),
        ] {
            std::fs::write(&path, &source).unwrap();
            let rust = command(&client, &["source-file", "tilde.conf"]);
            let c = oracle.run(&["source-file", "tilde.conf"]);
            assert!(!c.status.success());
            let expected = if source.contains("tmuxx_no_such") {
                "syntax error"
            } else {
                "user name is too long"
            };
            assert_eq!(rust.status, Some(1));
            assert_eq!(rust.error, None);
            assert_eq!(rust.output, c.stdout);
            assert!(c.stderr.is_empty(), "{c:?}");
            assert!(
                String::from_utf8_lossy(&rust.output)
                    .trim_end()
                    .ends_with(expected),
                "{rust:?}"
            );
        }
    }
}
#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn command_blocks_keep_parse_time_aliases_and_delayed_formats_like_c() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    for args in [
        vec![
            "set-option",
            "-s",
            "command-alias[90]",
            "late=display-message -p original",
        ],
        vec![
            "set-option",
            "-s",
            "command-alias[91]",
            "choose=if-shell -F 1",
        ],
    ] {
        assert_reference(&client, &oracle, &args);
    }
    for (name, source) in [
        (
            "blocks-formats.conf",
            "set -g @value before\nrun-shell -C -d 0.01 { set -g @value after; display -p 'block:#{@value}:#{command}' }\nrun-shell -C -d 0.01 \"set -g @value second; display -p 'string:#{@value}:#{command}'\"\n",
        ),
        (
            "blocks-env.conf",
            "PREP=parsed\nsetenv -g PREP runtime\nif-shell true { display -p \"block:$PREP\" }\nif-shell true 'display -p \"string:$PREP\"'\nif-shell -F 0 { display -p not-selected } { display -p \"else:$PREP\" }\n",
        ),
        (
            "blocks-aliases.conf",
            "set -s command-alias[90] 'late=display-message -p changed'\nif-shell -F 1 { late block }\nif-shell -F 1 'late string'\nchoose { late forwarded }\nrun-shell -C { late nested }\n",
        ),
        (
            "blocks-hooks.conf",
            "set-hook -g after-rename-window { run-shell -C { set -g @value hook; set -gF @observed '#{@value}:#{command}' } }\nshow-hooks -g after-rename-window\nrename-window -t main:0 renamed\nshow-options -gv @observed\nset-hook -gu after-rename-window\n",
        ),
        (
            "blocks-invalid.conf",
            "set -g @notrun bad; if-shell -F 0 { no-such-command }; display -p skipped\n",
        ),
    ] {
        std::fs::write(fixture.dir.join(name), source).unwrap();
        assert_reference(&client, &oracle, &["source-file", name]);
    }
    assert_reference(&client, &oracle, &["show-options", "-gqv", "@notrun"]);
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn source_flags_globs_alias_preparation_and_shell_validation_match_c() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    for (name, contents) in [
        ("dry.conf", "PREPARED=present\nset -g @dry bad\n"),
        ("00.conf", "set -g @glob zero\n"),
        ("01.conf", "set -ga @glob one\n"),
        ("alias.conf", "say prepared\n"),
    ] {
        std::fs::write(fixture.dir.join(name), contents).unwrap();
    }
    for args in [
        vec!["source-file", "-n", "dry.conf"],
        vec!["show-environment", "-g", "PREPARED"],
        vec!["show-options", "-gqv", "@dry"],
        vec!["set", "-g", "@path", "0*.conf"],
        vec!["source-file", "-F", "#{@path}"],
        vec!["show-options", "-gv", "@glob"],
        vec!["source-file", "-q", "missing*.conf"],
        vec![
            "set-option",
            "-s",
            "command-alias[9]",
            "say=display-message -p",
        ],
        vec!["say", "literal ; value"],
        vec!["source-file", "alias.conf"],
        vec!["set-option", "-g", "default-shell", "/tmuxx/does-not-exist"],
        vec!["set-option", "-g", "default-shell", "relative"],
        vec!["set-option", "-g", "default-shell", ""],
        vec!["show-options", "-gv", "default-shell"],
    ] {
        assert_reference(&client, &oracle, &args);
    }
}

#[test]
fn source_dry_run_validates_registration_without_running_commands_or_assignments() {
    let fixture = Fixture::new();
    let client = fixture.client();
    for source in [
        "unknown-command",
        "rename-session",
        "list-sessions extra",
        "list-sessions -z",
        "list-c",
    ] {
        std::fs::write(
            fixture.dir.join("validate.conf"),
            format!("DRY=wrong\nset-option -g @dry wrong\n{source}\n"),
        )
        .unwrap();
        let result = command(&client, &["source-file", "-n", "validate.conf"]);
        assert_eq!(result.error, None, "{source}");
        assert_eq!(result.status, Some(1), "{source}");
        assert!(String::from_utf8(result.output).unwrap().contains(":3: "));
        assert_eq!(ok(&client, &["show-options", "-gqv", "@dry"]).output, b"");
        assert!(
            command(&client, &["show-environment", "-g", "DRY"])
                .error
                .is_some()
        );
    }
    std::fs::write(
        fixture.dir.join("validate.conf"),
        b"%if 0\nunknown-command\n%endif\nlist-sess\nchoose-tree\n",
    )
    .unwrap();
    ok(&client, &["source-file", "-n", "validate.conf"]);
    assert!(
        command(&client, &["choose-tree"])
            .error
            .unwrap()
            .contains("unsupported")
    );
    ok(
        &client,
        &[
            "set-hook",
            "-g",
            "after-list-sessions",
            "set -g @retained yes",
        ],
    );
    assert!(
        command(
            &client,
            &["set-hook", "-g", "after-list-sessions", "rename-session"]
        )
        .error
        .is_some()
    );
    assert!(
        String::from_utf8(ok(&client, &["show-hooks", "-g", "after-list-sessions"]).output)
            .unwrap()
            .contains("@retained")
    );
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn source_dry_run_registration_and_skipped_branches_match_c() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    for source in [
        "unknown-command",
        "rename-session",
        "list-sessions extra",
        "list-sessions -z",
        "list-c",
        "list-sess",
        "%if 0\nunknown-command\n%endif\nchoose-tree",
    ] {
        std::fs::write(
            fixture.dir.join("validate.conf"),
            format!("DRY=wrong\nset-option -g @dry wrong\n{source}\n"),
        )
        .unwrap();
        assert_reference(&client, &oracle, &["source-file", "-n", "validate.conf"]);
        assert_reference(&client, &oracle, &["show-options", "-gqv", "@dry"]);
        assert_reference(&client, &oracle, &["show-environment", "-g", "DRY"]);
    }
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn source_line_groups_verbose_errors_and_outer_continuation_match_c() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    for (name, source) in [
        (
            "groups.conf",
            "set -g @order before; rename-session -t missing-session ignored; set -ga @order skipped\nset -ga @order after\ndisplay-message -p 'source=#{@order}'\n",
        ),
        ("invalid.conf", "set -g @notrun wrong\nunknown-command\n"),
        ("same-line.conf", "set -g @notrun wrong; unknown-command\n"),
        ("valid.conf", "set -g @included yes\ndisplay -p included\n"),
        ("outer.conf", "source-file invalid.conf\ndisplay -p outer\n"),
    ] {
        std::fs::write(fixture.dir.join(name), source).unwrap();
    }
    for args in [
        vec![
            "source-file",
            "groups.conf",
            ";",
            "display-message",
            "-p",
            "following",
        ],
        vec!["show-options", "-gv", "@order"],
        vec![
            "source-file",
            "-v",
            "groups.conf",
            ";",
            "display-message",
            "-p",
            "following",
        ],
        vec!["source-file", "-nv", "valid.conf"],
        vec![
            "source-file",
            "-v",
            "invalid.conf",
            "valid.conf",
            ";",
            "display-message",
            "-p",
            "following",
        ],
        vec!["source-file", "-v", "same-line.conf"],
        vec![
            "source-file",
            "outer.conf",
            ";",
            "display-message",
            "-p",
            "following",
        ],
        vec!["show-options", "-gqv", "@notrun"],
    ] {
        assert_reference(&client, &oracle, &args);
    }
}
#[test]
#[ignore = "requires current C oracle with event wait support in TMUXX_C_TMUX"]
fn event_wait_filter_nojobs_verbose_listing_and_wake_match_c() {
    let fixture = Fixture::new();
    let waiting = fixture.client();
    let admin = fixture.client();
    let oracle = reference(&fixture);
    ok(&admin, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    let args = [
        "wait-for",
        "-Ev",
        "-F",
        "#{&&:#{==:#{session},$2},#{&&:#{==:#{session_name},},1#(echo should-not-run)}}",
        "session-created",
        ";",
        "display-message",
        "-p",
        "matched",
    ];
    send(&waiting, 70, &args);
    let cwait = oracle.start(&args);
    let deadline = Instant::now() + Duration::from_secs(5);
    while oracle
        .run(&["wait-for", "-El", "session-created"])
        .stdout
        .is_empty()
    {
        assert!(Instant::now() < deadline);
    }
    for name in ["ignored", "accepted"] {
        assert_reference(
            &admin,
            &oracle,
            &["new-session", "-d", "-s", name, "sleep", "30"],
        );
    }
    let rust = reply(&waiting);
    let c = oracle.finish(cwait);
    assert_eq!(rust.error, None);
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    assert_eq!(rust.output, c.stdout);
    for (event, force) in [("@custom", false), ("@forced", true)] {
        send(
            &waiting,
            71,
            &[
                "wait-for",
                "-E",
                event,
                ";",
                "display-message",
                "-p",
                "woken",
            ],
        );
        let cwait = oracle.start(&[
            "wait-for",
            "-E",
            event,
            ";",
            "display-message",
            "-p",
            "woken",
        ]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let cname = loop {
            let listed = oracle.run(&["wait-for", "-El", event]);
            if !listed.stdout.is_empty() {
                break String::from_utf8(listed.stdout).unwrap();
            }
            assert!(Instant::now() < deadline);
        };
        let rname = String::from_utf8(ok(&admin, &["wait-for", "-El", event]).output).unwrap();
        assert_eq!(rname.lines().count(), cname.lines().count());
        if force {
            ok(&admin, &["wait-for", "-Ew", rname.trim(), event]);
            assert!(
                oracle
                    .run(&["wait-for", "-Ew", cname.trim(), event])
                    .status
                    .success()
            );
        } else {
            assert_reference(&admin, &oracle, &["set-hook", "-E", event]);
        }
        assert_eq!(reply(&waiting).output, oracle.finish(cwait).stdout);
    }
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn job_flags_delay_validation_cwd_and_target_context_match_c() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    #[cfg(windows)]
    ok(
        &client,
        &[
            "set-option",
            "-g",
            "default-shell",
            PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/cmd.exe")
                .to_str()
                .unwrap(),
        ],
    );
    for args in [
        vec!["set-option", "-g", "default-shell", "/bin/zsh"],
        vec!["set-option", "-s", "default-terminal", "vt220"],
        vec!["set-environment", "-g", "TERM", "outside"],
        vec!["set-environment", "-g", "COLORTERM", "outside"],
    ] {
        assert_reference(&client, &oracle, &args);
    }
    for args in [
        vec!["run", "-s", "ignored", "printf ignored-flag"],
        vec!["run", "-d", " 0", "printf leading-space"],
        vec!["run", "-d", "0x1p-9", "printf hex-float"],
        vec!["run", "-d", "", "printf empty-delay"],
        vec!["run", "-d", "0"],
        vec!["run", "-d", "0.001", ";", "display", "-p", "after-delay"],
        vec![
            "run",
            "-Ct",
            "main:0.0",
            "display -p '#{session_name}:#{pane_id}:#{command}'",
        ],
        vec![
            "if",
            "-Ft",
            "main:0.0",
            "1",
            "display -p '#{session_name}:#{pane_id}:#{command}'",
        ],
        vec!["run", "printf '%s|%s|%s' \"$0\" \"$TERM\" \"$COLORTERM\""],
        vec!["run", "-c", "/directory-that-does-not-exist/tmuxx", "pwd"],
        vec!["run", "-c", "/tmp", "pwd"],
        vec!["run", "-c", "#{pane_current_path}", "pwd"],
        vec!["if", "-Ft", "missing", "1", "display -p target-missing"],
        vec!["run", "-Ct", "missing", "display -p target-missing"],
    ] {
        assert_reference(&client, &oracle, &args);
    }
    for invalid in ["1 ", "oops", "0x1p-2garbage"] {
        let args = ["run", "-d", invalid, "printf forbidden"];
        let rust = command(&client, &args);
        let c = oracle.run(&args);
        assert!(!c.status.success());
        assert_eq!(
            rust.error
                .as_ref()
                .map(|error| format!("{error}\n").into_bytes()),
            Some(c.stderr),
            "{args:?}"
        );
        assert_eq!(rust.output, c.stdout);
    }
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn native_job_delay_bound_is_an_explicit_c_difference() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let oracle = reference(&fixture);
    ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
    for delay in ["0", "86400"] {
        assert_reference(&client, &oracle, &["run", "-b", "-d", delay, "true"]);
    }
    let args = [
        "run",
        "-b",
        "-d",
        "86400.001",
        "true",
        ";",
        "set",
        "-g",
        "@after-delay",
        "yes",
    ];
    let c = oracle.run(&args);
    assert!(c.status.success(), "{c:?}");
    assert!(c.stdout.is_empty() && c.stderr.is_empty());
    let rust = command(&client, &args);
    assert_eq!(
        rust.error.as_deref(),
        Some("delay outside 0..86400 seconds")
    );
    assert!(rust.output.is_empty());
    assert!(
        ok(&client, &["show", "-gqv", "@after-delay"])
            .output
            .is_empty()
    );
    assert_eq!(
        oracle.run(&["show", "-gqv", "@after-delay"]).stdout,
        b"yes\n"
    );
}

#[test]
#[ignore = "requires separate C oracle in TMUXX_C_TMUX"]
fn shell_job_stdin_socket_eof_and_signal_match_c() {
    #[cfg(windows)]
    return windows::job_stdin_and_termination();
    #[cfg(unix)]
    {
        let fixture = Fixture::new();
        let client = fixture.client();
        let oracle = reference(&fixture);
        ok(&client, &["new-session", "-d", "-s", "main", "sleep", "30"]);
        let script = "if test -S /dev/fd/0; then printf socket:; else printf other:; fi; if read line; then printf input; else printf eof; fi";
        assert_reference(&client, &oracle, &["run-shell", script]);
        assert_eq!(ok(&client, &["run-shell", script]).output, b"socket:eof\n");
        assert_reference(&client, &oracle, &["run-shell", "kill -TERM $$"]);
    }
}
