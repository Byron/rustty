#![cfg(windows)]

use std::{
    io::{Read, Write, pipe},
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tmuxx_protocol::native::{Request, ServerMessage};
use tmuxx_runtime::ServerHandle;
use windows_sys::Win32::{
    Foundation::*,
    System::{Pipes::*, Threading::CREATE_NO_WINDOW},
};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(host: &ServerHandle, argv: &[&str]) -> Vec<u8> {
    let client = host.connect().unwrap();
    client
        .send(Request {
            request_id: 1,
            argv: argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            stdin: None,
            subscribe: false,
            control: false,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{argv:?}");
            return reply.output;
        }
        assert!(
            Instant::now() < deadline,
            "native fixture command timed out: {argv:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn pipe_state(handle: HANDLE) -> (u32, u32) {
    let mut mode = 0;
    let mut flags = 0;
    unsafe {
        assert_ne!(
            GetNamedPipeHandleStateW(
                handle,
                &mut mode,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0
            ),
            0
        );
        assert_ne!(GetHandleInformation(handle, &mut flags), 0);
    }
    (mode, flags)
}

#[test]
fn silent_pipe_eof_and_cancel_close_control_input_without_changing_shared_handle_state() {
    let python = std::env::var("TMUXX_REMOTE_PYTHON").expect("explicit native Python interpreter");
    for (cancel, late_job) in [(false, false), (true, false), (false, true)] {
        let label = format!(
            "control-eof-{}-{}-{}",
            std::process::id(),
            cancel,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let endpoint = tmuxx_runtime::windows::endpoint(&label).unwrap();
        let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(endpoint)).unwrap();
        command(
            &host,
            &[
                "new-session",
                "-d",
                "-s",
                "silent",
                &python,
                "-c",
                "import time; time.sleep(60)",
            ],
        );
        let pane = command(&host, &["display-message", "-p", "#{pane_pid}"]);
        let (input, writer) = pipe().unwrap();
        let shared = input.try_clone().unwrap();
        let original = pipe_state(shared.as_raw_handle());
        assert_eq!(
            original.0 & PIPE_NOWAIT,
            0,
            "fixture starts in blocking pipe mode"
        );
        let mut client = OwnedChild(
            Command::new(env!("CARGO_BIN_EXE_tmuxx"))
                .args(["-L", &label, "-C", "attach-session", "-t", "silent"])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(input)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut output = client.0.stdout.take().unwrap();
        let mut errors = client.0.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            loop {
                let count = output.read(&mut bytes).unwrap();
                if count == 0 {
                    break;
                }
                if tx.send(bytes[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        let error_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            errors.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut bytes = Vec::new();
        while !bytes.windows(5).any(|part| part == b"%end ") {
            bytes.extend(
                rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("control attachment reply"),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            client.0.try_wait().unwrap().is_none(),
            "silent open writer prematurely ended control input"
        );
        let mut writer = Some(writer);
        if late_job {
            let job = format!("\"{python}\" -c \"import time; time.sleep(.2); print('LATE-JOB')\"");
            let source = format!(
                "display-message -p BEFORE-EOF\nrun-shell {}\n",
                String::from_utf8(tmuxx_command::quote_argument(job.as_bytes())).unwrap()
            );
            writer
                .as_mut()
                .unwrap()
                .write_all(source.as_bytes())
                .unwrap();
        }
        if cancel {
            command(&host, &["detach-client", "-s", "silent"]);
        } else {
            drop(writer.take());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = client.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "silent EOF/cancel must promptly release the control client"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(status.success(), "{status:?}");
        assert_eq!(
            pipe_state(shared.as_raw_handle()),
            original,
            "shared pipe mode/handle flags changed after EOF/cancel"
        );
        drop(writer);
        reader.join().unwrap();
        for part in rx.try_iter() {
            bytes.extend(part);
        }
        let lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with(b"%exit"))
                .count(),
            1,
            "every normal control close emits exactly one exit record: {bytes:?}"
        );
        if late_job {
            let before = bytes
                .windows(b"BEFORE-EOF".len())
                .position(|part| part == b"BEFORE-EOF")
                .expect("accepted immediate command output must drain before EOF");
            let exit = bytes
                .windows(b"%exit".len())
                .position(|part| part == b"%exit")
                .unwrap();
            assert!(before < exit);
            assert!(
                bytes[..exit]
                    .split(|byte| *byte == b'\n')
                    .filter(|line| line.starts_with(b"%end "))
                    .count()
                    >= 2,
                "attachment and immediate command guards close before exit"
            );
            std::thread::sleep(Duration::from_millis(350));
            assert!(
                !bytes
                    .windows(b"LATE-JOB".len())
                    .any(|part| part == b"LATE-JOB"),
                "deferred output must not revive a closed control client"
            );
        }
        assert_eq!(error_reader.join().unwrap(), b"");
        assert_eq!(
            command(&host, &["display-message", "-p", "#{pane_pid}"]),
            pane
        );
        assert_eq!(
            command(&host, &["list-clients", "-F", "#{client_name}"]),
            b""
        );
        host.shutdown().unwrap();
    }
}
