#![allow(dead_code)]
#[path = "../../../../conformance/remote_reference.rs"]
pub mod remote;
use remote::{RemoteControl, RemoteReference};
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::windows::process::ExitStatusExt,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tmuxx_protocol::{Decoder, Event, Notification};

pub const EXE: &str = env!("CARGO_BIN_EXE_tmuxx");
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Reference,
    Native,
}
pub const SIDES: [Side; 2] = [Side::Reference, Side::Native];

pub struct Fixture {
    pub directory: PathBuf,
    pub endpoint: PathBuf,
    pub reference: RemoteReference,
    pub host: Option<tmuxx_runtime::ServerHandle>,
    native_python: String,
}
impl Fixture {
    pub fn start() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let label = format!(
            "interop-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::temp_dir().join(&label);
        fs::create_dir(&directory).unwrap();
        let endpoint = tmuxx_runtime::windows::endpoint(&label).unwrap();
        let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(&endpoint)).unwrap();
        let fixture = Self {
            directory,
            endpoint,
            reference: RemoteReference::new(),
            host: Some(host),
            native_python: std::env::var("TMUXX_REMOTE_PYTHON")
                .expect("set the actual native Python executable in TMUXX_REMOTE_PYTHON"),
        };
        for side in SIDES {
            fixture.put(
                side,
                "interop_process.py",
                include_bytes!("../../../../conformance/interop_process.py"),
            );
            let output = fixture.run(
                side,
                &[
                    "new-session",
                    "-d",
                    "-s",
                    "test",
                    "-c",
                    &fixture.cwd(side),
                    "-x",
                    "80",
                    "-y",
                    "24",
                    fixture.python(side),
                    "-u",
                    "interop_process.py",
                    "idle",
                ],
                None,
            );
            assert!(
                output.status.success(),
                "{side:?} initial session: {output:?}"
            );
        }
        fixture
    }
    pub fn python(&self, side: Side) -> &str {
        if side == Side::Reference {
            "/usr/bin/python3"
        } else {
            &self.native_python
        }
    }
    pub fn cwd(&self, side: Side) -> String {
        if side == Side::Reference {
            String::from_utf8(self.reference.directory()).unwrap()
        } else {
            self.directory.to_str().unwrap().to_owned()
        }
    }
    pub fn child_command(&self, side: Side, mode: &str, extra: &[&str]) -> String {
        let path = self.path(side, "interop_process.py");
        let mut args = vec![self.python(side), "-u", &path, mode];
        args.extend_from_slice(extra);
        args.into_iter()
            .map(|value| {
                String::from_utf8(tmuxx_command::display_argument(value.as_bytes())).unwrap()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub fn native_command(&self) -> Command {
        let mut command = Command::new(EXE);
        command
            .arg("--native-endpoint")
            .arg(&self.endpoint)
            .current_dir(&self.directory)
            .env_remove("TMUX")
            .env_remove("TMUXX_ENDPOINT");
        command
    }
    pub fn run(&self, side: Side, args: &[&str], input: Option<&[u8]>) -> Output {
        if side == Side::Reference {
            self.reference.output(
                &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
                input.unwrap_or_default(),
            )
        } else {
            let mut child = OwnedChild(Some(
                self.native_command()
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            ));
            if let Some(input) = input {
                child
                    .0
                    .as_mut()
                    .unwrap()
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(input)
                    .unwrap();
            }
            child.0.as_mut().unwrap().stdin.take();
            wait_native(&mut child)
        }
    }
    pub fn path(&self, side: Side, leaf: &str) -> String {
        if side == Side::Reference {
            String::from_utf8(self.reference.path(leaf.as_bytes())).unwrap()
        } else {
            self.directory.join(leaf).to_str().unwrap().to_owned()
        }
    }
    pub fn put(&self, side: Side, leaf: &str, data: &[u8]) {
        if side == Side::Reference {
            self.reference.put(leaf.as_bytes(), data);
        } else {
            fs::write(self.directory.join(leaf), data).unwrap();
        }
    }
    pub fn exists(&self, side: Side, leaf: &str) -> bool {
        if side == Side::Reference {
            self.reference.exists(leaf.as_bytes())
        } else {
            self.directory.join(leaf).exists()
        }
    }
    pub fn get(&self, side: Side, leaf: &str) -> Vec<u8> {
        if side == Side::Reference {
            self.reference.get(leaf.as_bytes())
        } else {
            fs::read(self.directory.join(leaf)).unwrap()
        }
    }
    pub fn maybe_get(&self, side: Side, leaf: &str) -> Vec<u8> {
        if self.exists(side, leaf) {
            self.get(side, leaf)
        } else {
            Vec::new()
        }
    }
    pub fn control(&self, side: Side) -> ControlStream {
        ControlStream::start(self, side)
    }
    pub fn spawn(&self, side: Side, args: &[&str]) -> Pending {
        if side == Side::Reference {
            Pending::Reference(
                self.reference
                    .command_open_args(&args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>()),
            )
        } else {
            Pending::Native(OwnedChild(Some(
                self.native_command()
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            )))
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(host) = self.host.take() {
            let _ = host.shutdown();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub struct OwnedChild(pub Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn drain(mut input: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut data = Vec::new();
        input.read_to_end(&mut data).unwrap();
        data
    })
}
pub fn wait_native(child: &mut OwnedChild) -> Output {
    let value = child.0.as_mut().unwrap();
    let stdout = drain(value.stdout.take().unwrap());
    let stderr = drain(value.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = value.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = value.kill();
            let _ = value.wait();
            panic!("native CLI timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    child.0.take();
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}
pub enum Pending {
    Reference(RemoteControl),
    Native(OwnedChild),
}
impl Pending {
    pub fn id(&self) -> u32 {
        match self {
            Self::Reference(c) => c.pid,
            Self::Native(c) => c.0.as_ref().unwrap().id(),
        }
    }
    pub fn write(&mut self, data: &[u8]) {
        match self {
            Self::Reference(c) => {
                let out = c.write(data);
                assert!(out.status.success(), "{out:?}");
            }
            Self::Native(c) => {
                c.0.as_mut()
                    .unwrap()
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(data)
                    .unwrap()
            }
        }
    }
    pub fn finish_input(&mut self) {
        match self {
            Self::Reference(c) => {
                assert!(c.finish_input().status.success());
            }
            Self::Native(c) => {
                c.0.as_mut().unwrap().stdin.take();
            }
        }
    }
    pub fn exited(&mut self) -> Option<i32> {
        match self {
            Self::Reference(c) => c.exited(),
            Self::Native(c) => {
                c.0.as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .map(|status| status.code().unwrap_or(-1))
            }
        }
    }
    pub fn close(&mut self) {
        match self {
            Self::Reference(c) => c.close(),
            Self::Native(c) => {
                if let Some(child) = c.0.as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
    pub fn output(mut self) -> Output {
        match &mut self {
            Self::Native(c) => wait_native(c),
            Self::Reference(c) => {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                loop {
                    let output = c.read();
                    assert!(output.status.success(), "{output:?}");
                    let eof = output.stdout.is_empty();
                    stdout.extend(output.stdout);
                    stderr.extend(output.stderr);
                    if eof {
                        break;
                    }
                    assert!(Instant::now() < deadline, "C command output timeout");
                }
                let status = loop {
                    if let Some(status) = c.exited() {
                        break status;
                    }
                    assert!(Instant::now() < deadline, "C command exit timeout");
                    std::thread::sleep(Duration::from_millis(5));
                };
                c.close();
                Output {
                    status: ExitStatus::from_raw(status as u32),
                    stdout,
                    stderr,
                }
            }
        }
    }
}

enum ControlBackend {
    Reference {
        child: RemoteControl,
        decoder: RefCell<Decoder>,
        pending: RefCell<VecDeque<Event>>,
    },
    Native {
        child: OwnedChild,
        events: Receiver<Event>,
        reader: Option<JoinHandle<()>>,
        errors: Option<JoinHandle<Vec<u8>>>,
    },
}
pub struct ControlStream {
    side: Side,
    backend: ControlBackend,
}
impl ControlStream {
    pub fn start(fixture: &Fixture, side: Side) -> Self {
        let backend = if side == Side::Reference {
            ControlBackend::Reference {
                child: fixture.reference.control_open("test"),
                decoder: RefCell::new(Decoder::default()),
                pending: RefCell::new(VecDeque::new()),
            }
        } else {
            let mut child = OwnedChild(Some(
                fixture
                    .native_command()
                    .args(["-C", "attach-session", "-t", "test"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            ));
            let mut stdout = child.0.as_mut().unwrap().stdout.take().unwrap();
            let errors = Some(drain(child.0.as_mut().unwrap().stderr.take().unwrap()));
            let (tx, events) = mpsc::channel();
            let reader = Some(std::thread::spawn(move || {
                let mut decoder = Decoder::default();
                let mut data = [0; 8192];
                while let Ok(count) = stdout.read(&mut data) {
                    if count == 0 {
                        break;
                    }
                    for event in decoder.feed(&data[..count]).expect("native control bytes") {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                }
            }));
            ControlBackend::Native {
                child,
                events,
                reader,
                errors,
            }
        };
        let mut stream = Self { side, backend };
        stream.result();
        stream
    }
    pub fn write(&mut self, data: &[u8]) {
        match &mut self.backend {
            ControlBackend::Reference { child, .. } => {
                assert!(child.write(data).status.success());
            }
            ControlBackend::Native { child, .. } => child
                .0
                .as_mut()
                .unwrap()
                .stdin
                .as_mut()
                .unwrap()
                .write_all(data)
                .unwrap(),
        }
    }
    pub fn exited(&mut self) -> Option<i32> {
        match &mut self.backend {
            ControlBackend::Reference { child, .. } => child.exited(),
            ControlBackend::Native { child, .. } => child
                .0
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .map(|status| status.code().unwrap_or(-1)),
        }
    }
    pub fn next(&self) -> Event {
        match &self.backend {
            ControlBackend::Native { events, .. } => events
                .recv_timeout(Duration::from_secs(5))
                .expect("native control event deadline"),
            ControlBackend::Reference {
                child,
                decoder,
                pending,
            } => loop {
                if let Some(event) = pending.borrow_mut().pop_front() {
                    return event;
                }
                let output = child.read();
                assert!(
                    output.status.success() && !output.stdout.is_empty(),
                    "C control ended: {output:?}"
                );
                pending
                    .borrow_mut()
                    .extend(decoder.borrow_mut().feed(&output.stdout).unwrap());
            },
        }
    }
    pub fn result(&mut self) -> (Vec<Vec<u8>>, bool) {
        let mut lines = Vec::new();
        loop {
            match self.next() {
                Event::CommandOutput(line) => lines.push(line),
                Event::End { failed, .. } => return (lines, failed),
                _ => {}
            }
        }
    }
    pub fn command(&mut self, line: &str) -> (Vec<Vec<u8>>, bool) {
        self.write(format!("{line}\n").as_bytes());
        let result = self.result();
        if std::env::var_os("TMUXX_INTEROP_TRACE").is_some() {
            eprintln!("{:?} {line} -> {:?}", self.side, result);
        }
        result
    }
    pub fn ok(&mut self, line: &str) -> Vec<Vec<u8>> {
        let (lines, failed) = self.command(line);
        assert!(!failed, "{:?}: {line}: {lines:?}", self.side);
        lines
    }
    pub fn notification(&self, select: impl Fn(&Notification) -> bool) -> Notification {
        loop {
            if let Event::Notification(notification) = self.next()
                && select(&notification)
            {
                return notification;
            }
        }
    }
    pub fn records(&mut self, command: &str) -> Vec<Event> {
        self.records_until(command, "__native_control_barrier", true)
    }
    pub fn records_until(&mut self, command: &str, marker_text: &str, barrier: bool) -> Vec<Event> {
        self.write(format!("{command}\n").as_bytes());
        if barrier {
            self.write(format!("display -p {marker_text}\n").as_bytes());
        }
        let mut records = Vec::new();
        let mut marker = false;
        let mut number = 0;
        let mut previous = None;
        loop {
            let mut event = self.next();
            let complete = matches!(&event, Event::End { .. }) && marker;
            match &mut event {
                Event::Begin(guard) => {
                    assert!(previous.is_none_or(|old| guard.number > old));
                    previous = Some(guard.number);
                    number += 1;
                    guard.time = 0;
                    guard.number = number;
                }
                Event::End { guard, .. } => {
                    guard.time = 0;
                    guard.number = number;
                }
                Event::CommandOutput(line) if line == marker_text.as_bytes() => marker = true,
                Event::Notification(Notification::Output { .. }) => continue,
                _ => {}
            }
            records.push(event);
            if complete {
                return records;
            }
        }
    }
}
impl Drop for ControlStream {
    fn drop(&mut self) {
        match &mut self.backend {
            ControlBackend::Reference { child, .. } => {
                if !std::thread::panicking() {
                    child.close();
                }
            }
            ControlBackend::Native {
                child,
                reader,
                errors,
                ..
            } => {
                if let Some(mut child) = child.0.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                if let Some(reader) = reader.take() {
                    let _ = reader.join();
                }
                if let Some(errors) = errors.take() {
                    let bytes = errors.join().unwrap();
                    if !std::thread::panicking() {
                        assert!(bytes.is_empty(), "native control stderr: {bytes:?}");
                    }
                }
            }
        }
    }
}
