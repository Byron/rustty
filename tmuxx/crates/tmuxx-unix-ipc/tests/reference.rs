#![cfg(unix)]

//! Opt-in live tests: TMUXX_C_TMUX=/path/to/tmux TMUXX_C_SCHEMA=modern
//! TMUXX_C_VERSION=8 cargo test -p tmuxx-unix-ipc --test reference -- --ignored

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tmuxx_protocol::{Decoder, Event, Guard, encode_command_result};
use tmuxx_unix_ipc::{
    Connection, Identity, Schema, accept_client, flags, identify, msg, pack_argv, read_i32,
    run_command,
};

#[cfg(target_os = "linux")]
#[path = "../../../test-support/socket_owner.rs"]
mod socket_owner;

struct Directory(PathBuf, bool);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(format!(
            "/tmp/tmuxx-imsg-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path, false)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.1 || std::thread::panicking() {
            eprintln!("Preserved IPC fixture: {}", self.0.display());
        } else {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Reference {
    #[cfg(not(target_os = "linux"))]
    binary: PathBuf,
    socket: PathBuf,
    _directory: Directory,
    #[cfg(target_os = "linux")]
    owner: socket_owner::SocketOwner,
}
impl Reference {
    fn start() -> Self {
        let binary = binary();
        let directory = Directory::new();
        let socket = directory.0.join("server");
        let output = Command::new(&binary)
            .args(["-f", "/dev/null", "-S"])
            .arg(&socket)
            .args([
                "new-session",
                "-d",
                "-s",
                "reference",
                "-x",
                "80",
                "-y",
                "24",
                "sleep 30",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        #[cfg(target_os = "linux")]
        let owner = socket_owner::SocketOwner::capture(&socket, &binary, None).unwrap();
        Self {
            #[cfg(not(target_os = "linux"))]
            binary,
            socket,
            #[cfg(target_os = "linux")]
            owner,
            _directory: directory,
        }
    }
    fn assert_owner(&self) {
        #[cfg(target_os = "linux")]
        self.owner.check().unwrap();
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let cleanup_error = self.owner.terminate().err();
            self._directory.1 =
                cleanup_error.is_some() || self.owner.endpoint_replaced().unwrap_or(true);
            if let Some(error) = cleanup_error {
                eprintln!("C reference cleanup: {error}");
                assert!(
                    std::thread::panicking(),
                    "C reference cleanup failed: {error}"
                );
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = Command::new(&self.binary)
            .args(["-N", "-S"])
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}

fn binary() -> PathBuf {
    std::env::var_os("TMUXX_C_TMUX")
        .expect("set TMUXX_C_TMUX")
        .into()
}
fn schema() -> Schema {
    let version = std::env::var("TMUXX_C_VERSION")
        .unwrap_or_else(|_| "8".into())
        .parse()
        .unwrap();
    match std::env::var("TMUXX_C_SCHEMA")
        .unwrap_or_else(|_| "modern".into())
        .as_str()
    {
        "modern" => Schema::Modern { version },
        "legacy" => Schema::Legacy { version },
        _ => panic!("TMUXX_C_SCHEMA must be modern or legacy"),
    }
}
fn timed(stream: UnixStream) -> UnixStream {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}
fn args(values: &[&[u8]]) -> Vec<Vec<u8>> {
    values.iter().map(|value| value.to_vec()).collect()
}

#[test]
#[ignore = "requires an explicit C reference binary and schema"]
fn rust_command_client_against_c_server() {
    let reference = Reference::start();
    let run = |argv: &[&[u8]], input: &[u8]| {
        reference.assert_owner();
        run_command(
            &reference.socket,
            schema(),
            &args(argv),
            input,
            Duration::from_secs(5),
            1_000_000,
        )
        .unwrap()
    };
    let result = run(&[b"display-message", b"-p", b"wire \\ bytes"], &[]);
    assert_eq!(result.status, 0);
    assert_eq!(result.stdout, b"wire \\ bytes\n");
    let result = run(&[b"not-a-command"], &[]);
    assert_ne!(result.status, 0);
    assert!(!result.stderr.is_empty());
    assert_eq!(run(&[b"load-buffer", b"-"], b"binary\0\xff\n").status, 0);
    assert_eq!(run(&[b"save-buffer", b"-"], &[]).stdout, b"binary\0\xff\n");
}

#[test]
#[ignore = "requires an explicit C reference binary and schema"]
fn rust_control_client_against_c_server() {
    let reference = Reference::start();
    reference.assert_owner();
    let mut connection = Connection::new(
        timed(UnixStream::connect(&reference.socket).unwrap()),
        schema(),
    )
    .unwrap();
    let (input, mut writer) = UnixStream::pair().unwrap();
    let (reader, output) = UnixStream::pair().unwrap();
    let mut reader = timed(reader);
    identify(
        &mut connection,
        &Identity::current(true).unwrap(),
        Some(input.as_fd()),
        Some(output.as_fd()),
    )
    .unwrap();
    connection
        .send(
            msg::COMMAND,
            &pack_argv(&args(&[b"attach-session", b"-t", b"reference"])).unwrap(),
            None,
        )
        .unwrap();
    drop(input);
    drop(output);
    let task = std::thread::spawn(move || {
        loop {
            let message = connection
                .recv()
                .unwrap()
                .expect("control peer disappeared");
            match message.kind {
                msg::FLAGS => {}
                msg::EXIT | msg::SHUTDOWN => return,
                msg::DETACH | msg::DETACHKILL => connection.send(msg::EXITING, &[], None).unwrap(),
                msg::EXITED => return,
                _ => panic!("unexpected control IPC message {}", message.kind),
            }
        }
    });
    writer
        .write_all(b"display-message -p CONTROL-ROUNDTRIP\n")
        .unwrap();
    let mut bytes = Vec::new();
    let mut decoder = Decoder::default();
    let mut found = false;
    while !found {
        let mut chunk = [0; 1024];
        let length = reader.read(&mut chunk).unwrap();
        assert_ne!(length, 0);
        bytes.extend_from_slice(&chunk[..length]);
        decoder
            .feed_into(&chunk[..length], |event| {
                if matches!(event, Event::CommandOutput(line) if line == b"CONTROL-ROUNDTRIP") {
                    found = true;
                }
            })
            .unwrap();
    }
    writer.shutdown(std::net::Shutdown::Write).unwrap();
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).unwrap();
    decoder.feed(&tail).unwrap();
    decoder.finish().unwrap();
    task.join().unwrap();
}

fn accept_with_timeout(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return timed(stream),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

#[test]
#[ignore = "requires an explicit C reference binary and schema"]
fn c_command_client_against_rust_transport() {
    let directory = Directory::new();
    let socket = directory.0.join("server");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = ChildGuard(Some(
        Command::new(binary())
            .args(["-N", "-S"])
            .arg(&socket)
            .arg("list-sessions")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let mut connection = Connection::new(
        accept_with_timeout(&listener),
        Schema::Auto {
            version: schema().version(),
        },
    )
    .unwrap();
    let accepted = accept_client(&mut connection).unwrap();
    assert_eq!(accepted.argv, args(&[b"list-sessions"]));
    assert_eq!(connection.schema(), schema());
    assert!(accepted.stdin.is_some() && accepted.stdout.is_some());
    drop(accepted);
    connection
        .send(
            msg::WRITE_OPEN,
            &[
                1_i32.to_ne_bytes(),
                1_i32.to_ne_bytes(),
                0_i32.to_ne_bytes(),
            ]
            .concat(),
            None,
        )
        .unwrap();
    let ready = connection.recv().unwrap().unwrap();
    assert_eq!(ready.kind, msg::WRITE_READY);
    assert_eq!(read_i32(&ready.payload, 4).unwrap(), 0);
    connection
        .send(
            msg::WRITE,
            &[1_i32.to_ne_bytes().as_slice(), b"rust transport\n"].concat(),
            None,
        )
        .unwrap();
    connection
        .send(msg::EXIT, &0_i32.to_ne_bytes(), None)
        .unwrap();
    drop(connection);
    let output = child.0.take().unwrap().wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(output.stdout, b"rust transport\n");
}

#[test]
#[ignore = "requires an explicit C reference binary and schema"]
fn c_control_client_against_rust_transport() {
    let directory = Directory::new();
    let socket = directory.0.join("server");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = ChildGuard(Some(
        Command::new(binary())
            .args(["-N", "-S"])
            .arg(&socket)
            .args(["-C", "attach-session"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    child
        .0
        .as_mut()
        .unwrap()
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"display-message -p test\n")
        .unwrap();
    let mut connection = Connection::new(
        accept_with_timeout(&listener),
        Schema::Auto {
            version: schema().version(),
        },
    )
    .unwrap();
    let accepted = accept_client(&mut connection).unwrap();
    assert_ne!(accepted.identity.flags & flags::CONTROL, 0);
    assert_eq!(connection.schema(), schema());
    let mut input = fs::File::from(accepted.stdin.unwrap());
    let mut output = fs::File::from(accepted.stdout.unwrap());
    let mut command = [0; 24];
    input.read_exact(&mut command).unwrap();
    assert_eq!(&command, b"display-message -p test\n");
    output
        .write_all(&encode_command_result(
            Guard {
                time: 1,
                number: 44,
                flags: 1,
            },
            b"test",
            false,
        ))
        .unwrap();
    connection
        .send(msg::EXIT, &0_i32.to_ne_bytes(), None)
        .unwrap();
    drop(input);
    drop(output);
    drop(connection);
    let result = child.0.take().unwrap().wait_with_output().unwrap();
    assert!(result.status.success(), "{:?}", result.stderr);
    let mut decoder = Decoder::default();
    let events = decoder.feed(&result.stdout).unwrap();
    decoder.finish().unwrap();
    assert!(events.contains(&Event::CommandOutput(b"test".to_vec())));
}
