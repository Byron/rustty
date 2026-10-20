//! Native owner side of the original control contract matrix. Only the C
//! reference and its real -C clients run on the explicitly pinned oracle host.
#[path = "../../../../conformance/remote_reference.rs"]
mod remote_reference;
use super::{Decoder, Event, Notification};
use remote_reference::{RemoteControl, RemoteReference};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tmuxx_protocol::native::{ControlRequest, Request, ServerMessage};
use tmuxx_runtime::{ClientContext, LocalClient, ServerHandle};

struct Mapping {
    local: String,
    remote: String,
    names: BTreeMap<i32, String>,
    files: Vec<(String, String)>,
}
fn replace_bytes(input: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
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
fn quoted(value: &str) -> String {
    String::from_utf8(tmuxx_command::quote_argument(value.as_bytes())).unwrap()
}
fn python() -> String {
    std::env::var("TMUXX_REMOTE_PYTHON")
        .expect("set the native Python executable for process fixtures")
}
impl Mapping {
    fn source(&self, source: &str, native: bool, oracle: &RemoteReference) -> String {
        let mut result = source.to_owned();
        for (logical, actual) in &self.names {
            result = result.replace(&format!("client-{logical}"), actual);
        }
        let root = if native { &self.local } else { &self.remote };
        result = result.replace(
            "'/tmp/#{session_name}'",
            &quoted(&format!("{root}/#{{session_name}}")),
        );
        for (local, remote) in &self.files {
            if result.contains(local) {
                let data = std::fs::read(local).unwrap();
                let leaf = remote.rsplit('/').next().unwrap();
                assert_eq!(oracle.put(leaf.as_bytes(), &data), remote.as_bytes());
                if !native {
                    result = result.replace(local, remote);
                }
            }
        }
        if native {
            result = result.replace(
                "'sleep 120'",
                &format!(
                    "{} -c {}",
                    quoted(&python()),
                    quoted("import time; time.sleep(120)")
                ),
            );
            result = result.replace(
                "run-shell 'printf job; exit 3'",
                "run-shell 'echo|set /p=job& exit /b 3'",
            );
            result = result.replace("if-shell 'true'", "if-shell 'exit /b 0'");
        }
        result
    }
    fn canonical(&self, bytes: &[u8]) -> Vec<u8> {
        // Only these two disjoint, owned fixture roots differ between hosts.
        // Match exact bytes, preserving every other byte, diagnostic and order.
        let mut bytes = bytes.to_vec();
        for (local, remote) in &self.files {
            let leaf = remote.rsplit('/').next().unwrap();
            let canonical = format!("<fixture>/{leaf}");
            bytes = replace_bytes(&bytes, local.as_bytes(), canonical.as_bytes());
            bytes = replace_bytes(
                &bytes,
                local.replace('/', "\\").as_bytes(),
                canonical.as_bytes(),
            );
            bytes = replace_bytes(&bytes, remote.as_bytes(), canonical.as_bytes());
        }
        let bytes = replace_bytes(
            &bytes,
            b"'echo|set /p=job& exit /b 3' returned 3",
            b"'printf job; exit 3' returned 3",
        );
        let bytes = replace_bytes(&bytes, self.local.as_bytes(), b"<fixture>");
        let bytes = replace_bytes(
            &bytes,
            self.local.replace('/', "\\").as_bytes(),
            b"<fixture>",
        );
        replace_bytes(&bytes, self.remote.as_bytes(), b"<fixture>")
    }
    fn event(&self, event: &mut Event) {
        match event {
            Event::CommandOutput(value)
            | Event::UnframedOutput(value)
            | Event::Notification(Notification::ConfigError(value)) => {
                *value = self.canonical(value)
            }
            _ => {}
        }
    }
    fn argv(&self, args: &[&str], native: bool) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        for argument in args {
            let mut argument = argument.to_string();
            for (logical, actual) in &self.names {
                argument = argument.replace(&format!("client-{logical}"), actual);
            }
            if native && argument == "sleep 120" {
                result.extend([
                    python().into_bytes(),
                    b"-c".to_vec(),
                    b"import time; time.sleep(120)".to_vec(),
                ]);
            } else if native && argument == "printf '\\033kosc-name\\033\\\\'; sleep 120" {
                result.extend([
                    python().into_bytes(),
                    b"-c".to_vec(),
                    br#"import os,time; os.write(1,b'\x1bkosc-name\x1b\\'); time.sleep(120)"#
                        .to_vec(),
                ]);
            } else {
                result.push(argument.into_bytes());
            }
        }
        result
    }
}

pub(super) struct Pair {
    pub(super) rust: PathBuf,
    host: Option<ServerHandle>,
    oracle: RemoteReference,
    mapping: Rc<RefCell<Mapping>>,
    directory: PathBuf,
}
impl Pair {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("tx-control-{}-{serial}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let endpoint =
            tmuxx_runtime::windows::endpoint(&format!("control-{}-{serial}", std::process::id()))
                .unwrap();
        let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(endpoint)).unwrap();
        let oracle = RemoteReference::new();
        let anchor = String::from_utf8(oracle.path(b"anchor")).unwrap();
        let remote = anchor.strip_suffix("/anchor").unwrap().to_owned();
        let local = directory.to_str().unwrap().replace('\\', "/");
        let rust = PathBuf::from(format!("{local}/native"));
        let mapping = Rc::new(RefCell::new(Mapping {
            local: local.clone(),
            remote: remote.clone(),
            names: BTreeMap::new(),
            files: ["conf", "control.conf"]
                .into_iter()
                .map(|extension| {
                    (
                        rust.with_extension(extension).to_str().unwrap().to_owned(),
                        format!("{remote}/native.{extension}"),
                    )
                })
                .collect(),
        }));
        let pair = Self {
            rust,
            host: Some(host),
            oracle,
            mapping,
            directory,
        };
        for name in ["b", "a", "c"] {
            pair.command(&["new-session", "-d", "-s", name, "-n", "one", "sleep 120"]);
        }
        pair
    }
    pub(super) fn command(&self, args: &[&str]) -> Vec<u8> {
        let mapping = self.mapping.borrow();
        let c_args = mapping.argv(args, false);
        let mut c_args = c_args.iter().map(Vec::as_slice).collect::<Vec<_>>();
        if args.first() == Some(&"new-session") {
            c_args.splice(1..1, [b"-c".as_slice(), mapping.remote.as_bytes()]);
        }
        let c = self.oracle.output(&c_args, &[]);
        let rust = self
            .host
            .as_ref()
            .unwrap()
            .connect_with_context(ClientContext {
                cwd: Some(mapping.local.clone()),
                ..Default::default()
            })
            .unwrap();
        rust.send(Request {
            request_id: 1,
            argv: mapping.argv(args, true),
            stdin: None,
            subscribe: false,
            control: false,
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = loop {
            if let Ok(ServerMessage::Reply(value)) = rust.try_recv() {
                break value;
            }
            assert!(Instant::now() < deadline, "native command timeout {args:?}");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(
            result.status.unwrap_or(i32::from(result.error.is_some())),
            c.status.code().unwrap_or(1),
            "status {args:?}: {result:?} / {c:?}"
        );
        let error = result
            .error
            .map(|value| format!("{value}\n"))
            .unwrap_or_default();
        assert_eq!(
            mapping.canonical(error.as_bytes()),
            mapping.canonical(&c.stderr),
            "error {args:?}"
        );
        assert_eq!(
            mapping.canonical(&result.output),
            mapping.canonical(&c.stdout),
            "output {args:?}"
        );
        c.stdout
    }
    pub(super) fn clients(&self, logical: i32, session: &str) -> (Control<'_>, Control<'_>) {
        let remote = self.oracle.control_open(session);
        let name = String::from_utf8(remote.name.clone()).unwrap();
        {
            let mut mapping = self.mapping.borrow_mut();
            assert!(!mapping.names.values().any(|old| old == &name));
            assert!(mapping.names.insert(logical, name.clone()).is_none());
        }
        let client = self
            .host
            .as_ref()
            .unwrap()
            .connect_with_context(ClientContext {
                cwd: Some(self.mapping.borrow().local.clone()),
                client_name: name,
                identity: BTreeMap::from([("client_pid".into(), remote.pid.to_string())]),
                ..Default::default()
            })
            .unwrap();
        let mut rust = Control::new(Some(client), None, &self.oracle, self.mapping.clone());
        rust.native
            .as_ref()
            .unwrap()
            .send_control(ControlRequest {
                request_id: 1,
                argv: vec![
                    b"attach-session".to_vec(),
                    b"-t".to_vec(),
                    session.as_bytes().to_vec(),
                ],
                source: None,
                subscribe: true,
            })
            .unwrap();
        let mut c = Control::new(None, Some(remote), &self.oracle, self.mapping.clone());
        assert!(rust.reply().is_empty());
        assert!(c.reply().is_empty());
        for control in [&mut rust, &mut c] {
            assert_eq!(control.run("display -p ready"), b"ready\n");
            control.notifications.clear();
        }
        (rust, c)
    }
}
impl Drop for Pair {
    fn drop(&mut self) {
        // Release all native child cwd handles before removing private files.
        // The leased oracle independently owns its C fixture descendants.
        drop(self.host.take());
        let removed = std::fs::remove_dir_all(&self.directory);
        if !std::thread::panicking() {
            removed.expect("remove released native control fixture");
        }
    }
}

pub(super) struct Control<'a> {
    native: Option<LocalClient>,
    remote: Option<RemoteControl>,
    oracle: &'a RemoteReference,
    mapping: Rc<RefCell<Mapping>>,
    decoder: Decoder,
    request: u64,
    completion: u64,
    snapshot: Option<(String, u64)>,
    detached: Option<(String, bool)>,
    pub(super) notifications: Vec<Notification>,
    pub(super) failed: bool,
}
impl<'a> Control<'a> {
    fn new(
        native: Option<LocalClient>,
        remote: Option<RemoteControl>,
        oracle: &'a RemoteReference,
        mapping: Rc<RefCell<Mapping>>,
    ) -> Self {
        Self {
            native,
            remote,
            oracle,
            mapping,
            decoder: Decoder::default(),
            request: 1,
            completion: 0,
            snapshot: None,
            detached: None,
            notifications: Vec::new(),
            failed: false,
        }
    }
    fn write(&mut self, source: &str) {
        let source = self
            .mapping
            .borrow()
            .source(source, self.native.is_some(), self.oracle);
        self.request += 1;
        if let Some(client) = &self.native {
            client
                .send_control(ControlRequest {
                    request_id: self.request,
                    argv: vec![],
                    source: Some(source.into_bytes()),
                    subscribe: false,
                })
                .unwrap();
        } else {
            let result = self
                .remote
                .as_ref()
                .unwrap()
                .write(format!("{source}\n").as_bytes());
            assert!(result.status.success(), "{result:?}");
        }
    }
    fn read(&mut self) -> Vec<u8> {
        if let Some(client) = &self.native {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match client.try_recv() {
                    Ok(ServerMessage::Control { mut data }) => {
                        data.push(b'\n');
                        return data;
                    }
                    Ok(ServerMessage::Detached { session, hangup }) => {
                        self.detached = Some((session, hangup))
                    }
                    Ok(ServerMessage::ClientFlags { .. }) => {}
                    Ok(ServerMessage::Reply(response)) => {
                        // Native batch completion is separate from the C-compatible
                        // per-command guards. It must remain ordered and unique.
                        assert!(response.request_id > self.completion);
                        assert!(response.request_id <= self.request);
                        self.completion = response.request_id;
                    }
                    Ok(ServerMessage::Snapshot {
                        instance_id,
                        sequence,
                        ..
                    }) => {
                        if let Some((previous, old_sequence)) = &self.snapshot {
                            assert_eq!(&instance_id, previous);
                            assert!(sequence >= *old_sequence);
                        }
                        self.snapshot = Some((instance_id, sequence));
                    }
                    Ok(ServerMessage::Changed { sequence }) => {
                        if let Some((_, previous)) = &self.snapshot {
                            assert!(sequence >= *previous);
                        }
                    }
                    Ok(ServerMessage::PaneOutput { pane_id, data, .. }) => {
                        return tmuxx_protocol::encode_output(pane_id, &data);
                    }
                    Ok(other) => panic!(
                        "unexpected native control message {:?}",
                        std::mem::discriminant(&other)
                    ),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return Vec::new(),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                assert!(Instant::now() < deadline, "native control read deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let result = self.remote.as_ref().unwrap().read();
        assert!(result.status.success(), "{result:?}");
        result.stdout
    }
    fn decode(&mut self) -> Vec<Event> {
        let bytes = self.read();
        assert!(!bytes.is_empty(), "control EOF before requested record");
        let mut events = self.decoder.feed(&bytes).unwrap();
        for event in &mut events {
            self.mapping.borrow().event(event);
        }
        events
    }
    fn reply(&mut self) -> Vec<u8> {
        let mut result = Vec::new();
        loop {
            let mut ended = false;
            for event in self.decode() {
                match event {
                    Event::CommandOutput(line) => {
                        result.extend(line);
                        result.push(b'\n');
                    }
                    Event::End { failed, .. } => {
                        self.failed = failed;
                        ended = true;
                    }
                    Event::Notification(notice) => self.notifications.push(notice),
                    _ => {}
                }
            }
            if ended {
                return result;
            }
        }
    }
    pub(super) fn run(&mut self, command: &str) -> Vec<u8> {
        self.write(command);
        self.reply()
    }
    pub(super) fn records(&mut self, command: &str) -> Vec<Event> {
        self.records_until(command, "__tmuxx_control_barrier", true)
    }
    pub(super) fn records_until(
        &mut self,
        command: &str,
        marker_text: &str,
        barrier: bool,
    ) -> Vec<Event> {
        self.write(command);
        if barrier {
            self.write(&format!("display -p {marker_text}"));
        }
        let mut records = Vec::new();
        let mut marker = false;
        let mut complete = false;
        let mut number = 0;
        let mut previous = None;
        while !complete {
            for mut event in self.decode() {
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
                        if marker {
                            complete = true;
                        }
                    }
                    Event::CommandOutput(line) if line == marker_text.as_bytes() => marker = true,
                    Event::Notification(Notification::Output { .. }) => continue,
                    _ => {}
                }
                records.push(event);
            }
        }
        records
    }
    pub(super) fn detached(&mut self, hangup: bool, session: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        if let Some(client) = &self.native {
            while self.detached.is_none() {
                if let Ok(ServerMessage::Detached { session, hangup }) = client.try_recv() {
                    self.detached = Some((session, hangup));
                }
                assert!(Instant::now() < deadline, "native detach deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(self.detached.as_ref(), Some(&(session.to_owned(), hangup)));
        } else {
            loop {
                let events = self.decode();
                if let Some(reason) = events.into_iter().find_map(|event| match event {
                    Event::Notification(Notification::Exit { reason }) => Some(reason),
                    _ => None,
                }) {
                    // A real C -C pipe prints bare %exit. Its Unix imsg
                    // DETACHKILL/EXITING handshake remains tested separately.
                    assert_eq!(reason, None);
                    break;
                }
                assert!(Instant::now() < deadline, "C detach record deadline");
            }
            while self.remote.as_ref().unwrap().exited().is_none() {
                assert!(Instant::now() < deadline, "C detach process deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(self.remote.as_ref().unwrap().exited(), Some(0));
        }
    }
}
