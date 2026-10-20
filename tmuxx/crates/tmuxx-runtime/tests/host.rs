#![cfg(any(unix, windows))]

use std::io::{BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};
use tmuxx_protocol::native::{Request, Response, ServerMessage};

fn fixture_argv(args: &[&str]) -> Vec<Vec<u8>> {
    let values = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    #[cfg(windows)]
    let mut values = values;
    #[cfg(windows)]
    if let Some(index) = values.iter().position(|arg| arg == "/bin/sh") {
        let source = match values.last().unwrap().as_str() {
            "printf READY; sleep 30" => "import time; print('READY',flush=True); time.sleep(30)",
            "printf 'READY\\n'; read value; printf 'DONE:%s\\n' \"$value\"; sleep 30" => {
                "import time; print('READY',flush=True); value=input(); print('DONE:'+value,flush=True); time.sleep(30)"
            }
            source => panic!("missing native counterpart for fixture source: {source}"),
        };
        values[index] = std::env::var("TMUXX_REMOTE_PYTHON").unwrap_or_else(|_| "python".into());
        *values.last_mut().unwrap() = source.into();
    }
    values.iter().map(|arg| arg.as_bytes().to_vec()).collect()
}

#[cfg(unix)]
fn connect(path: &std::path::Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(path)
}
#[cfg(windows)]
fn connect(path: &std::path::Path) -> std::io::Result<tmuxx_runtime::windows::PipeStream> {
    tmuxx_runtime::windows::connect(path)
}

fn endpoint() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[cfg(windows)]
    return tmuxx_runtime::windows::endpoint(&format!(
        "host-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap();
    #[cfg(unix)]
    std::env::temp_dir().join(format!(
        "tmuxx-{}-{}-{}.native",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn command(endpoint: &Path, args: &[&str]) -> Response {
    let mut stream = connect(endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    tmuxx_runtime::native_handshake(&mut stream).unwrap();
    let request = Request {
        request_id: 1,
        stdin: None,
        argv: fixture_argv(args),
        subscribe: false,
        control: false,
    };
    serde_json::to_writer(&mut stream, &request).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut reader = BufReader::new(stream);
    let line = tmuxx_runtime::read_record(&mut reader, 32 * 1024 * 1024)
        .unwrap()
        .unwrap();
    match serde_json::from_slice(&line).unwrap() {
        ServerMessage::Reply(response) => response,
        message => panic!("unexpected message: {message:?}"),
    }
}

#[test]
fn disconnect_keeps_same_process_and_input_still_works() {
    let path = endpoint();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    let created = command(
        &path,
        &[
            "new-session",
            "-d",
            "-s",
            "test",
            "-x",
            "30",
            "-y",
            "5",
            "/bin/sh",
            "-c",
            "printf 'READY\\n'; read value; printf 'DONE:%s\\n' \"$value\"; sleep 30",
        ],
    );
    assert_eq!(created.error, None);
    let pid = command(&path, &["display-message", "-p", "#{pane_pid}"]).output;
    assert!(!pid.is_empty());
    assert_eq!(
        command(&path, &["display-message", "-p", "#{pane_pid}"]).output,
        pid
    );
    assert_eq!(command(&path, &["send-keys", "-l", "hello"]).error, None);
    assert_eq!(command(&path, &["send-keys", "Enter"]).error, None);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let captured = command(&path, &["capture-pane", "-p"]);
        if String::from_utf8_lossy(&captured.output).contains("DONE:hello") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "child did not receive input: {captured:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    host.shutdown().unwrap();
    assert!(!path.exists());
}

#[test]
fn dropping_app_host_stops_the_server() {
    let path = endpoint();
    {
        let _host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
        assert!(connect(&path).is_ok());
    }
    assert!(!path.exists());
    assert!(connect(&path).is_err());
}

#[test]
fn refuses_to_replace_existing_endpoint() {
    let path = endpoint();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    assert!(tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).is_err());
    assert!(connect(&path).is_ok());
    host.shutdown().unwrap();
}

#[test]
#[cfg(unix)]
fn cleanup_does_not_remove_a_replacement_endpoint() {
    let path = endpoint();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    host.shutdown().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    std::fs::remove_file(path).unwrap();
}

#[test]
#[cfg(unix)]
fn stale_owned_socket_is_recovered_but_regular_files_are_preserved() {
    let path = endpoint();
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    assert!(connect(&path).is_ok());
    host.shutdown().unwrap();
    std::fs::write(&path, b"not a socket").unwrap();
    assert!(tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn native_handshake_rejects_mismatched_versions_before_requests() {
    use std::io::Read;
    let path = endpoint();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    let mut stream = connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"{\"version\":4294967295}\n{\"request_id\":1,\"argv\":[]}\n")
        .unwrap();
    let mut data = Vec::new();
    stream.read_to_end(&mut data).unwrap();
    let hello = serde_json::from_slice::<tmuxx_protocol::native::Hello>(&data).unwrap();
    #[cfg(windows)]
    let hello = {
        let mut hello = hello;
        assert!(
            hello
                .instance_id
                .take()
                .is_some_and(|value| !value.is_empty())
        );
        hello
    };
    assert_eq!(
        hello,
        tmuxx_protocol::native::Hello {
            stdin_file_stream: true,
            client_features: Some(0),
            ..Default::default()
        }
    );
    assert!(command(&path, &["has-session"]).error.is_some());
    host.shutdown().unwrap();
}

#[test]
fn tokenized_groups_keep_literals_and_error_boundaries() {
    use tmuxx_command::{Command, parse_argv};
    let commands = vec![
        Command {
            blocks: Default::default(),
            args: vec![
                b"display-message".to_vec(),
                b"-p".to_vec(),
                b"literal;".to_vec(),
            ],
        },
        Command {
            blocks: Default::default(),
            args: vec![
                b"display-message".to_vec(),
                b"-p".to_vec(),
                b"trailing\\;".to_vec(),
            ],
        },
    ];
    assert_eq!(
        parse_argv(tmuxx_runtime::command_argv(&commands)).unwrap(),
        commands
    );
}

#[test]
fn native_socket_streams_snapshots_and_cap_rejection_keeps_the_process() {
    for limit in [16, tmuxx_protocol::native::MAX_SNAPSHOT_BYTES] {
        let path = endpoint();
        let mut options = tmuxx_runtime::Options::new(&path);
        options.snapshot_bytes = limit;
        let host = tmuxx_runtime::start(options).unwrap();
        assert_eq!(
            command(
                &path,
                &[
                    "new-session",
                    "-d",
                    "-s",
                    "snapshot",
                    "/bin/sh",
                    "-c",
                    "printf READY; sleep 30"
                ]
            )
            .error,
            None
        );
        let pid = command(&path, &["display-message", "-p", "#{pane_pid}"]).output;
        let mut stream = connect(&path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        tmuxx_runtime::native_handshake(&mut stream).unwrap();
        serde_json::to_writer(
            &mut stream,
            &Request {
                request_id: 1,
                argv: vec![],
                stdin: None,
                subscribe: true,
                control: false,
            },
        )
        .unwrap();
        stream.write_all(b"\n").unwrap();
        let mut reader = BufReader::new(stream);
        let mut assembler = tmuxx_protocol::native::SnapshotAssembler::default();
        let mut started = false;
        loop {
            let bytes = tmuxx_runtime::read_record(&mut reader, 512 * 1024)
                .unwrap()
                .unwrap();
            let message: ServerMessage = serde_json::from_slice(&bytes).unwrap();
            started |= matches!(message, ServerMessage::SnapshotStart { .. });
            let Some(message) = assembler.push(message).unwrap() else {
                continue;
            };
            match message {
                ServerMessage::Snapshot { state, .. } => {
                    assert!(started);
                    assert_ne!(limit, 16);
                    assert_eq!(
                        state["panes"][0]["pid"].as_u64().unwrap().to_string(),
                        String::from_utf8_lossy(&pid).trim()
                    );
                    break;
                }
                ServerMessage::SnapshotAborted { error, .. } => {
                    assert_eq!(limit, 16);
                    assert!(error.contains("snapshot exceeds byte limit"));
                    break;
                }
                _ => {}
            }
        }
        assembler.finish().unwrap();
        drop(reader);
        assert_eq!(
            command(&path, &["display-message", "-p", "#{pane_pid}"]).output,
            pid
        );
        host.shutdown().unwrap();
    }
}

#[test]
#[cfg(unix)]
fn second_endpoint_failure_removes_only_owned_bindings() {
    use std::os::unix::net::UnixListener;
    let native = endpoint();
    let ipc = endpoint();
    let listener = UnixListener::bind(&ipc).unwrap();
    let mut options = tmuxx_runtime::Options::new(&native);
    options.tmux_endpoint = Some(ipc.clone());
    assert!(tmuxx_runtime::start(options).is_err());
    assert!(!native.exists());
    assert!(connect(&ipc).is_ok());
    drop(listener);
    std::fs::remove_file(ipc).unwrap();
}

#[test]
#[cfg(unix)]
fn shutdown_terminates_pty_descendants_after_the_leader_exits() {
    let other_path = endpoint();
    let other = tmuxx_runtime::start(tmuxx_runtime::Options::new(&other_path)).unwrap();
    assert_eq!(
        command(&other_path, &["new-session", "-d", "sleep", "30"]).error,
        None
    );
    let other_pid = command(&other_path, &["display-message", "-p", "#{pane_pid}"]).output;
    let path = endpoint();
    let pidfile = path.with_extension("descendant");
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    let script = "import os,signal,time,sys\npid=os.fork()\nif pid == 0:\n os.setpgid(0,0)\n signal.signal(signal.SIGHUP,signal.SIG_IGN)\n open(sys.argv[1],'w').write(str(os.getpid()))\n os.write(1,b'READY')\n time.sleep(60)\nelse:\n time.sleep(.1)\n os._exit(0)";
    assert_eq!(
        command(
            &path,
            &[
                "new-session",
                "-d",
                "python3",
                "-c",
                script,
                pidfile.to_str().unwrap()
            ]
        )
        .error,
        None
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Ok(pid) = std::fs::read_to_string(&pidfile)
            && let Ok(pid) = pid.parse::<libc::pid_t>()
        {
            break pid;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    std::thread::sleep(Duration::from_millis(200));
    host.shutdown().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 {
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            panic!("PTY descendant survived shutdown after its leader exited");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_file(pidfile).unwrap();
    assert_eq!(
        command(&other_path, &["display-message", "-p", "#{pane_pid}"]).output,
        other_pid
    );
    other.shutdown().unwrap();
}

#[test]
fn socket_clients_are_bounded_and_shutdown_closes_idle_peers() {
    use std::io::Read;
    let path = endpoint();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    let mut clients = Vec::new();
    for _ in 0..tmuxx_runtime::MAX_SOCKET_CLIENTS {
        let mut stream = connect(&path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        tmuxx_runtime::native_handshake(&mut stream).unwrap();
        clients.push(stream);
    }
    let mut excess = connect(&path).unwrap();
    excess
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert!(tmuxx_runtime::native_handshake(&mut excess).is_err());
    drop(clients.pop());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut stream = connect(&path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        if tmuxx_runtime::native_handshake(&mut stream).is_ok() {
            clients.push(stream);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "closed client did not release its slot"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    host.shutdown().unwrap();
    for mut stream in clients {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
    }
}

#[test]
#[ignore = "requires C reference in TMUXX_C_TMUX"]
#[cfg(unix)]
fn pty_descendant_cleanup_difference_matches_c() {
    use std::process::Command;
    struct Descendant(libc::pid_t);
    impl Drop for Descendant {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
    for detached_session in [false, true] {
        for rust in [false, true] {
            let socket = endpoint();
            let pidfile = socket.with_extension("ownership");
            let script = format!(
                "import os,signal,time,sys\npid=os.fork()\nif pid == 0:\n {}\n signal.signal(signal.SIGHUP,signal.SIG_IGN)\n open(sys.argv[1],'w').write(str(os.getpid()))\n os.write(1,b'READY')\n time.sleep(60)\nelse:\n time.sleep(.1)\n os._exit(0)",
                if detached_session {
                    "os.setsid()"
                } else {
                    "os.setpgid(0,0)"
                }
            );
            let host =
                rust.then(|| tmuxx_runtime::start(tmuxx_runtime::Options::new(&socket)).unwrap());
            let argv = [
                "new-session",
                "-d",
                "python3",
                "-c",
                &script,
                pidfile.to_str().unwrap(),
            ];
            if rust {
                assert_eq!(command(&socket, &argv).error, None);
            } else {
                let result = Command::new(std::env::var_os("TMUXX_C_TMUX").unwrap())
                    .args(["-f", "/dev/null", "-S"])
                    .arg(&socket)
                    .args(argv)
                    .args([";", "set-option", "-s", "exit-empty", "off"])
                    .output()
                    .unwrap();
                assert!(result.status.success(), "{result:?}");
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            let child = loop {
                if let Ok(pid) = std::fs::read_to_string(&pidfile)
                    && let Ok(pid) = pid.parse::<libc::pid_t>()
                {
                    break Descendant(pid);
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            };
            std::thread::sleep(Duration::from_millis(200));
            if let Some(host) = host {
                host.shutdown().unwrap();
            } else {
                let result = Command::new(std::env::var_os("TMUXX_C_TMUX").unwrap())
                    .arg("-S")
                    .arg(&socket)
                    .arg("kill-server")
                    .output()
                    .unwrap();
                assert!(result.status.success(), "{result:?}");
            }
            let should_survive = !rust || detached_session;
            let deadline = Instant::now() + Duration::from_secs(2);
            while !should_survive
                && unsafe { libc::kill(child.0, 0) } == 0
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                unsafe { libc::kill(child.0, 0) } == 0,
                should_survive,
                "rust={rust}, detached_session={detached_session}"
            );
            std::fs::remove_file(pidfile).unwrap();
        }
    }
}

#[cfg(windows)]
#[path = "host/windows.rs"]
mod windows;

#[test]
#[ignore = "requires pinned remote C reference in TMUXX_REMOTE_C_CONFIG"]
#[cfg(windows)]
fn pty_descendant_cleanup_difference_matches_c() {
    windows::pty_descendant_cleanup_difference_matches_c();
}

#[test]
fn native_stdin_detection_uses_command_flags_and_positional_paths() {
    for (argv, expected) in [
        (vec!["source", "-"], true),
        (vec!["source-file", "-nv", "before", "-", "after"], true),
        (vec!["source-file", "-t", "-", "file"], false),
        (vec!["source-file", "-Z", "-"], false),
        (vec!["loadb", "-bname", "-"], true),
        (vec!["load-buffer", "-b", "-", "file"], false),
        (vec!["display", "-p", "-"], false),
        (vec!["display", "-p", "before", ";", "source", "-"], true),
    ] {
        let argv = argv
            .into_iter()
            .map(|arg| arg.as_bytes().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(
            tmuxx_runtime::command_reads_stdin(&argv),
            expected,
            "{argv:?}"
        );
    }
}

#[test]
#[cfg(windows)]
fn cleanup_does_not_remove_a_replacement_endpoint() {
    let path = endpoint();
    let owner = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    let old_peer = connect(&path).unwrap();
    owner.shutdown().unwrap();
    let replacement = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    drop(old_peer);
    assert_eq!(
        command(&path, &["display", "-p", "replacement"]).output,
        b"replacement\n"
    );
    replacement.shutdown().unwrap();
}

#[test]
#[cfg(windows)]
fn stale_owned_socket_is_recovered_but_regular_files_are_preserved() {
    use std::os::windows::{
        ffi::OsStrExt,
        io::{FromRawHandle, OwnedHandle},
    };
    use windows_sys::Win32::{Foundation::INVALID_HANDLE_VALUE, System::Pipes::*};
    let path = endpoint();
    let name = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let stale = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            3,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(stale, INVALID_HANDLE_VALUE);
    // Closing the abandoned owner's kernel handle removes stale pipe discovery.
    drop(unsafe { OwnedHandle::from_raw_handle(stale) });
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    assert!(connect(&path).is_ok());
    host.shutdown().unwrap();
    let file = std::env::temp_dir().join(format!("host-preserved-{}.txt", std::process::id()));
    std::fs::write(&file, b"not a pipe").unwrap();
    assert!(tmuxx_runtime::start(tmuxx_runtime::Options::new(&file)).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"not a pipe");
    std::fs::remove_file(file).unwrap();
}

#[test]
#[cfg(windows)]
fn second_endpoint_failure_removes_only_owned_bindings() {
    let native = endpoint();
    let existing = endpoint();
    let owner = tmuxx_runtime::start(tmuxx_runtime::Options::new(&existing)).unwrap();
    let mut options = tmuxx_runtime::Options::new(&native);
    options.tmux_endpoint = Some(existing.clone());
    assert!(tmuxx_runtime::start(options).is_err());
    assert!(connect(&native).is_err());
    assert_eq!(
        command(&existing, &["display", "-p", "independent"]).output,
        b"independent\n"
    );
    owner.shutdown().unwrap();
}

#[test]
#[cfg(windows)]
fn shutdown_terminates_pty_descendants_after_the_leader_exits() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::*;
    let other_path = endpoint();
    let other = tmuxx_runtime::start(tmuxx_runtime::Options::new(&other_path)).unwrap();
    assert_eq!(
        command(
            &other_path,
            &[
                "new-session",
                "-d",
                "python",
                "-c",
                "import time; time.sleep(30)"
            ]
        )
        .error,
        None
    );
    let other_pid = command(&other_path, &["display-message", "-p", "#{pane_pid}"]).output;
    let path = endpoint();
    let pidfile = std::env::temp_dir().join(format!("host-descendant-{}.pid", std::process::id()));
    let release = pidfile.with_extension("release");
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap();
    let script = "import pathlib,subprocess,sys,time\nchild=subprocess.Popen([sys.executable,'-c','import signal,time; signal.signal(signal.SIGINT,signal.SIG_IGN); signal.signal(signal.SIGBREAK,signal.SIG_IGN); time.sleep(60)'],creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)\npathlib.Path(sys.argv[1]).write_text(str(child.pid))\nwhile not pathlib.Path(sys.argv[2]).exists(): time.sleep(.01)";
    assert_eq!(
        command(
            &path,
            &[
                "new-session",
                "-d",
                "python",
                "-c",
                script,
                pidfile.to_str().unwrap(),
                release.to_str().unwrap()
            ]
        )
        .error,
        None
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&pidfile)
            && let Ok(pid) = text.parse::<u32>()
        {
            break pid;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!handle.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
        258
    );
    std::fs::write(&release, b"exit").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    host.shutdown().unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 3000) },
        0
    );
    assert_eq!(
        command(&other_path, &["display-message", "-p", "#{pane_pid}"]).output,
        other_pid
    );
    other.shutdown().unwrap();
    std::fs::remove_file(pidfile).unwrap();
    std::fs::remove_file(release).unwrap();
}
