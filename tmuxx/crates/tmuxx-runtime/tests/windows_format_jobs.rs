#![cfg(windows)]
//! The original format-job matrix with native child programs and a real C oracle.
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tmuxx_protocol::{
    Decoder, Event,
    native::{Request, ServerMessage},
};
use tmuxx_runtime::{ClientContext, LocalClient, Options, ServerHandle};

#[path = "../../../conformance/remote_reference.rs"]
mod remote;
use remote::{RemoteControl, RemoteReference};

struct Oracle {
    control: RemoteControl,
    decoder: Decoder,
    events: VecDeque<Event>,
}
impl Oracle {
    fn reply(&mut self) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut output = Vec::new();
        loop {
            while let Some(event) = self.events.pop_front() {
                match event {
                    Event::CommandOutput(line) => {
                        output.extend(line);
                        output.push(b'\n');
                    }
                    Event::End { failed, .. } => {
                        assert!(!failed, "C control command failed: {output:?}");
                        return output;
                    }
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "C control reply deadline");
            let result = self.control.read();
            assert!(result.status.success(), "C control read: {result:?}");
            assert!(result.stderr.is_empty(), "C stderr: {:?}", result.stderr);
            self.events
                .extend(self.decoder.feed(&result.stdout).unwrap());
        }
    }
    fn run(&mut self, args: &[&str]) -> Vec<u8> {
        // Only the private child executable spelling differs between hosts.
        // Results, including each host's placeholder spelling, remain exact.
        let fixture_args = args
            .iter()
            .map(|value| {
                value.replace(
                    "python format_jobs_fixture.py",
                    "/usr/bin/python3 format_jobs_fixture.py",
                )
            })
            .collect::<Vec<_>>();
        let mut command = tmuxx_command::serialize(&[tmuxx_command::Command {
            blocks: Default::default(),
            args: fixture_args
                .iter()
                .map(|value| value.as_bytes().to_vec())
                .collect(),
        }]);
        command.push(b'\n');
        let result = self.control.write(&command);
        assert!(result.status.success(), "C control write: {result:?}");
        self.events
            .extend(self.decoder.feed(&result.stdout).unwrap());
        self.reply()
    }
}

struct Pair {
    oracle: Oracle,
    client: Option<LocalClient>,
    host: Option<ServerHandle>,
    reference: RemoteReference,
    dir: PathBuf,
}
impl Pair {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "fjob-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(format!("tmuxx-{name}"));
        std::fs::create_dir(&dir).unwrap();
        let script = include_bytes!("../../../conformance/format_jobs_fixture.py");
        std::fs::write(dir.join("format_jobs_fixture.py"), script).unwrap();
        let reference = RemoteReference::new();
        let path = reference.put(b"format_jobs_fixture.py", script);
        let remote_dir = &path[..path.iter().rposition(|byte| *byte == b'/').unwrap()];
        let control = reference.control_open_args(&[
            b"new-session",
            b"-s",
            b"main",
            b"-n",
            b"fixed",
            b"-c",
            remote_dir,
            b"/usr/bin/python3 format_jobs_fixture.py idle",
        ]);
        let mut oracle = Oracle {
            control,
            decoder: Decoder::default(),
            events: VecDeque::new(),
        };
        assert!(oracle.reply().is_empty());
        let host = tmuxx_runtime::start(Options::new(
            tmuxx_runtime::windows::endpoint(&name).unwrap(),
        ))
        .unwrap();
        let client = host
            .connect_with_context(ClientContext {
                cwd: Some(dir.to_str().unwrap().to_owned()),
                ..ClientContext::default()
            })
            .unwrap();
        let mut pair = Self {
            oracle,
            client: Some(client),
            host: Some(host),
            reference,
            dir,
        };
        pair.rust(&[
            "new-session",
            "-s",
            "main",
            "-n",
            "fixed",
            "python format_jobs_fixture.py idle",
        ]);
        pair.oracle.run(&["set-environment", "-g", "SIDE", "c"]);
        pair.rust(&["set-environment", "-g", "SIDE", "r"]);
        pair
    }
    fn rust(&self, args: &[&str]) -> Vec<u8> {
        let client = self.client.as_ref().unwrap();
        client
            .send(Request {
                request_id: 1,
                argv: args.iter().map(|value| value.as_bytes().to_vec()).collect(),
                stdin: None,
                subscribe: false,
                control: true,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
                assert_eq!(reply.error, None, "{args:?}");
                assert_eq!(reply.status.unwrap_or(0), 0, "{args:?}");
                return reply.output;
            }
            assert!(Instant::now() < deadline, "Rust reply {args:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn run(&mut self, args: &[&str]) -> Vec<u8> {
        let c = self.oracle.run(args);
        let rust = self.rust(args);
        assert_eq!(rust, c, "{args:?}");
        rust
    }
    fn wait_format(&mut self, format: &str, expected: &[u8]) {
        self.wait_format_outputs(format, expected, expected);
    }
    fn wait_format_outputs(&mut self, format: &str, expected: &[u8], expected_c: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let c = self.oracle.run(&["display-message", "-p", format]);
            let rust = self.rust(&["display-message", "-p", format]);
            if c == expected_c && rust == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "format {format}: C {c:?}, Rust {rust:?}, expected {expected:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn write(&self, name: &str) {
        std::fs::write(self.dir.join(name), b"").unwrap();
        self.reference.put(name.as_bytes(), b"");
    }
}
impl Drop for Pair {
    fn drop(&mut self) {
        self.client.take();
        self.host.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
fn asynchronous_format_jobs_stream_cache_expand_and_restart_like_c() {
    let mut pair = Pair::new();
    pair.run(&["set-option", "-g", "@value", "first"]);
    let dynamic = "#(python format_jobs_fixture.py emit \"#{@value}\")";
    assert_eq!(pair.run(&["display-message", "-p", dynamic]), b"\n");
    pair.wait_format(dynamic, b"first\n");
    pair.run(&["set-option", "-g", "@value", "changed"]);
    pair.wait_format(dynamic, b"changed\n");
    pair.wait_format("#(python format_jobs_fixture.py expand)", b"changed\n");
    let stream = "#(python format_jobs_fixture.py stream)";
    assert_eq!(pair.run(&["display-message", "-p", stream]), b"\n");
    pair.wait_format(stream, b"first\n");
    pair.write("next");
    pair.wait_format(stream, b"second\n");
    pair.write("done");
    pair.wait_format(stream, b"tail\n");
    pair.wait_format("#(python format_jobs_fixture.py nested)", b"\n");
    pair.wait_format("#(python format_jobs_fixture.py failure)", b"out\n");
    pair.wait_format("#(python format_jobs_fixture.py emit \"#{session_name}:#(python format_jobs_fixture.py forbidden)\")", b"main:\n");
    assert!(!pair.dir.join("forbidden").exists());
    assert!(!pair.reference.exists(b"forbidden"));
}

#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
fn repeated_format_jobs_share_live_work_and_report_not_ready() {
    let mut pair = Pair::new();
    let command = "python format_jobs_fixture.py shared";
    let format = format!("#({command})");
    assert_eq!(
        pair.run(&["display-message", "-p", &format!("{format}|{format}")]),
        b"|\n"
    );
    pair.wait_format_outputs(
        &format,
        format!("<'{command}' not ready>\n").as_bytes(),
        b"<'/usr/bin/python3 format_jobs_fixture.py shared' not ready>\n",
    );
    assert_eq!(std::fs::read(pair.dir.join("r.count")).unwrap(), b"x");
    assert_eq!(pair.reference.get(b"c.count"), b"x");
    pair.write("release");
    pair.wait_format(&format, b"finished\n");
}

#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
fn format_jobs_stop_with_the_requesting_client() {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    let mut pair = Pair::new();
    assert_eq!(
        pair.oracle.run(&[
            "display-message",
            "-p",
            "#(exec python format_jobs_fixture.py lifetime)"
        ]),
        b"\n"
    );
    assert_eq!(
        pair.rust(&[
            "display-message",
            "-p",
            "#(python format_jobs_fixture.py lifetime)"
        ]),
        b"\n"
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while !pair.dir.join("r.pid").exists() || !pair.reference.exists(b"c.pid") {
        assert!(Instant::now() < deadline, "format jobs did not write PIDs");
        std::thread::sleep(Duration::from_millis(2));
    }
    let pid = std::fs::read_to_string(pair.dir.join("r.pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let c_pid = String::from_utf8(pair.reference.get(b"c.pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!process.is_null(), "open original native format job {pid}");
    struct HeldProcess(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for HeldProcess {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let process = HeldProcess(process);
    assert_ne!(unsafe { WaitForSingleObject(process.0, 0) }, WAIT_OBJECT_0);
    let c_watch = pair.reference.watch_process(c_pid);
    assert!(pair.reference.process_alive(c_watch));
    pair.run(&["detach-client"]);
    let deadline = Instant::now() + Duration::from_secs(8);
    while unsafe { WaitForSingleObject(process.0, 0) } != WAIT_OBJECT_0
        || pair.reference.process_alive(c_watch)
    {
        assert!(
            Instant::now() < deadline,
            "format job survived client: Windows {pid}, C {c_pid}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
