//! Stock tmux connections: one control stream per remote session.
//! Visible capture is best effort; the native transport has stronger snapshots.
use super::*;
use std::sync::atomic::AtomicUsize;
use tmuxx_client::State;
use tmuxx_client::system::{SystemClient, SystemEvent};

pub(super) struct SystemConnection {
    shared: Arc<Shared>,
    manager: Option<thread::JoinHandle<()>>,
}
impl SystemConnection {
    pub(super) fn view_size(&self, session: u64, cols: u16, rows: u16) -> io::Result<()> {
        if cols == 0 || rows == 0 {
            return Err(err("zero system tmux view size"));
        }
        send(
            &self.shared,
            vec![
                b"tmuxx-system-size".to_vec(),
                session.to_string().into_bytes(),
                format!("{cols},{rows}").into_bytes(),
            ],
        )
        .map(|_| ())
    }
    pub(super) fn close(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        if let Some(thread) = self.manager.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for SystemConnection {
    fn drop(&mut self) {
        self.close();
    }
}

impl Connection {
    /// Connect to an existing stock tmux server through the user's SSH setup.
    pub fn system_ssh(host: &str, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        if host.is_empty() {
            return Err(err("SSH host is empty"));
        }
        let (mut connection, requests) = Self::base(wake, true);
        let (events, receive) = mpsc::sync_channel(256);
        let primary = Actor::start(host, None, 0, events.clone())?;
        let shared = connection.shared.clone();
        let manager_shared = shared.clone();
        let host = host.to_owned();
        connection.source = Source::SystemSsh(host.clone());
        let manager = thread::Builder::new()
            .name("rustty-system-tmux".into())
            .spawn(move || {
                manage(host, manager_shared, requests, events, receive, primary);
            })?;
        connection.system = Some(SystemConnection {
            shared,
            manager: Some(manager),
        });
        Ok(connection)
    }
}

enum TransportEvent {
    Bytes(u64, Vec<u8>),
    Stderr(u64, Vec<u8>),
    Closed(u64),
}
struct Actor {
    session: Option<u64>,
    child: Child,
    writes: Option<mpsc::SyncSender<Vec<u8>>>,
    workers: Vec<thread::JoinHandle<()>>,
    client: SystemClient,
    stderr: Vec<u8>,
    sizes: Option<(u16, u16)>,
    direct_buffers: BTreeMap<u64, Vec<u8>>,
    cleanup_requests: std::collections::BTreeSet<u64>,
}
impl Actor {
    fn start(
        host: &str,
        session: Option<u64>,
        id: u64,
        events: mpsc::SyncSender<TransportEvent>,
    ) -> io::Result<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(err)?;
        let mut client = SystemClient::new(
            nonce
                .iter()
                .flat_map(|byte| format!("{byte:02x}").into_bytes())
                .collect(),
        )
        .map_err(err)?;
        client.bootstrap().map_err(err)?;
        let remote = session.map_or_else(
            || "exec tmux -C attach-session -f new-layouts".to_owned(),
            |id| format!("exec tmux -C attach-session -f new-layouts -t '${id}'"),
        );
        let mut child = Command::new("ssh")
            .args(["-T", "-o", "BatchMode=yes", "--", host])
            .arg(remote)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (writes, receiver) = mpsc::sync_channel::<Vec<u8>>(2);
        let read_events = events.clone();
        let read = thread::Builder::new()
            .name(format!("rustty-system-read-{id}"))
            .spawn(move || {
                let mut bytes = [0; 32768];
                loop {
                    match output.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            if read_events
                                .send(TransportEvent::Bytes(id, bytes[..n].to_vec()))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = read_events.send(TransportEvent::Closed(id));
            });
        let write = thread::Builder::new()
            .name(format!("rustty-system-write-{id}"))
            .spawn(move || {
                for bytes in receiver {
                    if input.write_all(&bytes).is_err() || input.flush().is_err() {
                        break;
                    }
                }
            });
        let errors = thread::Builder::new()
            .name(format!("rustty-system-stderr-{id}"))
            .spawn(move || {
                let mut bytes = [0; 4096];
                while let Ok(n) = stderr.read(&mut bytes) {
                    if n == 0
                        || events
                            .send(TransportEvent::Stderr(id, bytes[..n].to_vec()))
                            .is_err()
                    {
                        break;
                    }
                }
            });
        let mut workers = Vec::new();
        for result in [read, write, errors] {
            match result {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        let mut actor = Self {
            session,
            child,
            writes: Some(writes),
            workers,
            client,
            stderr: Vec::new(),
            sizes: None,
            direct_buffers: BTreeMap::new(),
            cleanup_requests: Default::default(),
        };
        actor.flush()?;
        Ok(actor)
    }
    fn direct(&mut self, pane: Vec<u8>, bytes: Vec<u8>) -> io::Result<()> {
        if bytes.is_empty() || bytes.contains(&0) || bytes.len() > 1024 * 1024 {
            return Err(err("invalid stock tmux direct input"));
        }
        if pane.first() != Some(&b'%')
            || pane.len() < 2
            || !pane[1..].iter().all(u8::is_ascii_digit)
        {
            return Err(err("stock tmux direct input requires a pane ID"));
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(err)?;
        let name = format!(
            "__rustty_input_{}",
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
        .into_bytes();
        let commands = vec![
            tmuxx_command::Command {
                blocks: Default::default(),
                args: vec![
                    b"set-buffer".to_vec(),
                    b"-b".to_vec(),
                    name.clone(),
                    b"--".to_vec(),
                    bytes,
                ],
            },
            tmuxx_command::Command {
                blocks: Default::default(),
                args: vec![
                    b"paste-buffer".to_vec(),
                    b"-drS".to_vec(),
                    b"-b".to_vec(),
                    name.clone(),
                    b"-t".to_vec(),
                    pane,
                ],
            },
        ];
        let id = self.client.queue(commands).map_err(err)?;
        self.direct_buffers.insert(id, name);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        for bytes in self.client.take_writes() {
            self.writes
                .as_ref()
                .ok_or_else(|| err("SSH writer stopped"))?
                .try_send(bytes)
                .map_err(err)?;
        }
        Ok(())
    }
    fn close(mut self) -> Vec<thread::JoinHandle<()>> {
        drop(self.writes.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::take(&mut self.workers)
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        drop(self.writes.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn manage(
    host: String,
    shared: Arc<Shared>,
    requests: mpsc::Receiver<Request>,
    events: mpsc::SyncSender<TransportEvent>,
    receive: mpsc::Receiver<TransportEvent>,
    first: Actor,
) {
    let mut actors = BTreeMap::from([(0, first)]);
    let mut primary = 0;
    let mut next_actor = 1;
    let mut state = State::default();
    let mut retired = Vec::new();
    let mut topology = String::new();
    while !shared.closed.load(Ordering::Acquire) {
        let mut topology_changed = false;
        for _ in 0..256 {
            if shared.closed.load(Ordering::Acquire) {
                break;
            }
            let Ok(event) = receive.try_recv() else {
                break;
            };
            match event {
                TransportEvent::Stderr(id, bytes) => {
                    if let Some(actor) = actors.get_mut(&id) {
                        let count = bytes
                            .len()
                            .min(8192usize.saturating_sub(actor.stderr.len()));
                        actor.stderr.extend_from_slice(&bytes[..count]);
                    }
                }
                TransportEvent::Bytes(id, bytes) => {
                    let Some(actor) = actors.get_mut(&id) else {
                        continue;
                    };
                    let updates = actor.client.feed(&bytes);
                    actor.session = actor.client.state().attached_session.or(actor.session);
                    let session = actor.session;
                    for update in updates {
                        match update {
                            SystemEvent::State(next) if id == primary && !next.disconnected => {
                                state = next;
                                topology_changed = true;
                                publish_graph(&shared, &state);
                            }
                            SystemEvent::PaneCapture {
                                pane_id,
                                data,
                                cols,
                                rows,
                            } if owns(&state, session, pane_id) => {
                                if let Err(error) = capture(&shared, pane_id, data, cols, rows) {
                                    fail(&shared, error);
                                }
                                publish_graph(&shared, &state);
                            }
                            SystemEvent::PaneOutput { pane_id, data }
                                if owns(&state, session, pane_id) =>
                            {
                                if let Err(error) = live(&shared, pane_id, &data) {
                                    fail(&shared, error);
                                }
                            }
                            SystemEvent::CommandFinished {
                                request_id,
                                failed,
                                output,
                            } => {
                                if actor.cleanup_requests.remove(&request_id) {
                                    continue;
                                }
                                if let Some(name) = actor.direct_buffers.remove(&request_id)
                                    && failed
                                {
                                    // A live connection can remove a buffer left by a failed paste.
                                    if let Ok(id) =
                                        actor.client.queue(vec![tmuxx_command::Command {
                                            blocks: Default::default(),
                                            args: vec![
                                                b"delete-buffer".to_vec(),
                                                b"-b".to_vec(),
                                                name,
                                            ],
                                        }])
                                    {
                                        actor.cleanup_requests.insert(id);
                                    }
                                }
                                if failed {
                                    report(&shared, String::from_utf8_lossy(&output).trim());
                                }
                            }
                            SystemEvent::UnframedOutput { data } => {
                                report(&shared, String::from_utf8_lossy(&data));
                            }
                            SystemEvent::Indeterminate { request_id } => report(
                                &shared,
                                format!(
                                    "SSH disconnected before command {request_id} was confirmed; it was not retried"
                                ),
                            ),
                            SystemEvent::Error(error) => {
                                fail(&shared, error);
                                shared.closed.store(true, Ordering::Release);
                            }
                            _ => {}
                        }
                    }
                }
                TransportEvent::Closed(id) => {
                    if let Some(mut actor) = actors.remove(&id) {
                        for update in actor.client.finish() {
                            if let SystemEvent::Indeterminate { request_id } = update {
                                report(
                                    &shared,
                                    format!(
                                        "SSH command {request_id} has an unknown outcome; it was not retried"
                                    ),
                                );
                            }
                        }
                        let expected = actor
                            .session
                            .is_some_and(|id| !state.sessions.contains_key(&id));
                        if !expected {
                            report(
                                &shared,
                                format!(
                                    "SSH tmux connection closed: {}",
                                    String::from_utf8_lossy(&actor.stderr).trim()
                                ),
                            );
                        }
                        retired.extend(actor.close());
                    }
                    if id == primary {
                        if let Some((&id, actor)) = actors.first_key_value() {
                            primary = id;
                            state = actor.client.state().clone();
                            topology_changed = true;
                        } else {
                            fail(&shared, "SSH tmux disconnected");
                            shared.closed.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            }
        }
        if shared.closed.load(Ordering::Acquire) {
            break;
        }
        if topology_changed {
            let next_topology = format!(
                "{:?}:{:?}",
                state
                    .sessions
                    .iter()
                    .map(|(id, s)| (*id, &s.windows))
                    .collect::<Vec<_>>(),
                state
                    .panes
                    .iter()
                    .map(|(id, p)| (*id, p.window_id, p.width, p.height, p.left, p.top))
                    .collect::<Vec<_>>()
            );
            if !state.discovery_needed && topology != next_topology {
                topology = next_topology;
                for (id, actor) in &mut actors {
                    if *id != primary
                        && let Err(error) = actor.client.bootstrap()
                    {
                        report(&shared, error);
                    }
                }
            }
        }
        if state.sessions.len() > 64 {
            fail(
                &shared,
                "remote session count exceeds the 64-connection policy",
            );
            shared.closed.store(true, Ordering::Release);
            break;
        }
        // Each session has an independently attached stream. Shared windows are
        // fed by their lowest session ID exactly once.
        for &session in state.sessions.keys() {
            if actors.values().any(|actor| actor.session == Some(session)) {
                continue;
            }
            match Actor::start(&host, Some(session), next_actor, events.clone()) {
                Ok(actor) => {
                    actors.insert(next_actor, actor);
                    next_actor += 1;
                }
                Err(error) => {
                    fail(&shared, error);
                    shared.closed.store(true, Ordering::Release);
                    break;
                }
            }
        }
        if shared.closed.load(Ordering::Acquire) {
            break;
        }
        let gone = actors
            .iter()
            .filter_map(|(id, actor)| {
                (*id != primary
                    && actor
                        .session
                        .is_some_and(|s| !state.sessions.contains_key(&s)))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in gone {
            if let Some(actor) = actors.remove(&id) {
                retired.extend(actor.close());
            }
        }
        for request in requests.try_iter().take(256) {
            if request.argv.is_empty() {
                for actor in actors.values_mut() {
                    if let Err(error) = actor.client.bootstrap() {
                        report(&shared, error);
                    }
                }
                continue;
            }
            if request.argv[0] == b"tmuxx-system-direct" {
                let result = (|| -> io::Result<()> {
                    if request.argv.len() != 2 {
                        return Err(err("invalid direct input target"));
                    }
                    actors
                        .get_mut(&primary)
                        .ok_or_else(|| err("SSH control stream is not ready"))?
                        .direct(
                            request.argv[1].clone(),
                            request.stdin.ok_or_else(|| err("missing direct input"))?,
                        )
                })();
                if let Err(error) = result {
                    report(&shared, error);
                }
                continue;
            }
            if request.argv[0] == b"tmuxx-system-size" {
                let result = (|| -> io::Result<()> {
                    let session = std::str::from_utf8(
                        request.argv.get(1).ok_or_else(|| err("missing session"))?,
                    )
                    .map_err(err)?
                    .parse::<u64>()
                    .map_err(err)?;
                    let size = std::str::from_utf8(
                        request.argv.get(2).ok_or_else(|| err("missing size"))?,
                    )
                    .map_err(err)?;
                    let (cols, rows) = size.split_once(',').ok_or_else(|| err("invalid size"))?;
                    let size_pair = (
                        cols.parse::<u16>().map_err(err)?,
                        rows.parse::<u16>().map_err(err)?,
                    );
                    let actor = actors
                        .values_mut()
                        .find(|actor| actor.session == Some(session))
                        .ok_or_else(|| err("session control stream is not ready"))?;
                    if actor.sizes != Some(size_pair) {
                        actor
                            .client
                            .queue(vec![tmuxx_command::Command {
                                blocks: Default::default(),
                                args: vec![
                                    b"refresh-client".to_vec(),
                                    b"-C".to_vec(),
                                    size.as_bytes().to_vec(),
                                ],
                            }])
                            .map_err(err)?;
                        actor.sizes = Some(size_pair);
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    report(&shared, error);
                }
                continue;
            }
            let result = (|| -> io::Result<()> {
                if request.stdin.is_some() {
                    return Err(err(
                        "stock tmux control does not support native stdin payloads",
                    ));
                }
                let commands = tmuxx_command::parse_argv(&request.argv).map_err(err)?;
                actors
                    .get_mut(&primary)
                    .ok_or_else(|| err("SSH control stream is not ready"))?
                    .client
                    .queue(commands)
                    .map_err(err)?;
                Ok(())
            })();
            if let Err(error) = result {
                report(&shared, error);
            }
        }
        for actor in actors.values_mut() {
            if let Err(error) = actor.flush() {
                fail(&shared, error);
                shared.closed.store(true, Ordering::Release);
                break;
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    // Unblock reader workers before joining them during owner shutdown.
    drop(receive);
    for (_, actor) in actors {
        retired.extend(actor.close());
    }
    for worker in retired {
        let _ = worker.join();
    }
}

fn owns(state: &State, session: Option<u64>, pane_id: u64) -> bool {
    if session.is_none() {
        return false;
    }
    state.panes.get(&pane_id).and_then(|pane| {
        state
            .sessions
            .values()
            .filter(|s| s.windows.values().any(|id| *id == pane.window_id))
            .map(|s| s.id)
            .min()
    }) == session
}
fn report(shared: &Shared, error: impl std::fmt::Display) {
    if let Ok(mut model) = shared.model.lock() {
        if model.errors.len() == 128 {
            model.errors.remove(0);
        }
        model.errors.push(error.to_string());
    }
    (shared.wake)();
}
fn publish_graph(shared: &Shared, state: &State) {
    let Ok(mut model) = shared.model.lock() else {
        return;
    };
    if state.discovery_needed
        || state
            .panes
            .keys()
            .any(|pane| !model.panes.contains_key(pane))
    {
        return;
    }
    let graph = Graph {
        sessions: state
            .sessions
            .values()
            .map(|s| ServerSession {
                id: s.id,
                name: String::from_utf8_lossy(s.name.as_deref().unwrap_or_default()).into_owned(),
                active: u64::from(s.active_index.unwrap_or(0)),
                windows: s.windows.clone(),
            })
            .collect(),
        windows: state
            .windows
            .values()
            .map(|w| {
                let (cols, rows) = Layout::parse(&String::from_utf8_lossy(&w.layout))
                    .map(|layout| (layout.geometry.width, layout.geometry.height))
                    .unwrap_or_else(|_| {
                        w.panes.iter().filter_map(|id| state.panes.get(id)).fold(
                            (1, 1),
                            |(cols, rows), p| {
                                (
                                    cols.max(
                                        (i64::from(p.left) + i64::from(p.width))
                                            .clamp(1, u32::MAX as i64)
                                            as u32,
                                    ),
                                    rows.max(
                                        (i64::from(p.top) + i64::from(p.height))
                                            .clamp(1, u32::MAX as i64)
                                            as u32,
                                    ),
                                )
                            },
                        )
                    });
                ServerWindow {
                    id: w.id,
                    name: String::from_utf8_lossy(w.name.as_deref().unwrap_or_default())
                        .into_owned(),
                    active: w
                        .active_pane
                        .or_else(|| w.panes.first().copied())
                        .unwrap_or(0),
                    cols: cols.min(u16::MAX as u32) as u16,
                    rows: rows.min(u16::MAX as u32) as u16,
                    panes: w.panes.iter().copied().collect(),
                    layout: String::from_utf8_lossy(&w.layout).into_owned(),
                    zoomed: w.flags.contains(&b'Z'),
                    zoomed_pane: None,
                    visible_layout: String::from_utf8_lossy(&w.visible_layout).into_owned(),
                }
            })
            .collect(),
        panes: state
            .panes
            .values()
            .map(|p| ServerPane {
                id: p.id,
                window: p.window_id,
                x: Some(p.left),
                y: Some(p.top),
                cols: p.width.min(u16::MAX as u32) as u16,
                rows: p.height.min(u16::MAX as u32) as u16,
                pid: None,
                modal: p.modal,
                // Stock control formats do not expose these modal policies.
                modal_capture_keys: false,
                modal_close_click: false,
                modal_close_cancel: false,
                persistent: true,
                status: None,
                terminal: serde_json::Value::Null,
            })
            .collect(),
    };
    model.panes.retain(|id, pane| {
        if state.panes.contains_key(id) {
            true
        } else {
            for view in &pane.views {
                if let Some(exited) = view.exited.upgrade() {
                    exited.store(true, Ordering::Release);
                }
                (view.wake)();
            }
            false
        }
    });
    model.graph = Some(graph);
    (shared.wake)();
}
fn capture(shared: &Shared, pane_id: u64, data: Vec<u8>, cols: u32, rows: u32) -> io::Result<()> {
    let cols: u16 = cols.try_into().map_err(err)?;
    let rows: u16 = rows.try_into().map_err(err)?;
    let mut terminal = Terminal::new(cols, rows, 10_000);
    let _ = terminal.feed(&data);
    let mut model = shared.model.lock().map_err(err)?;
    let pane = model.panes.entry(pane_id).or_insert_with(|| PaneState {
        host_state: None,
        terminal: terminal.clone(),
        views: Vec::new(),
        owners: Arc::new(AtomicUsize::new(0)),
    });
    pane.terminal = terminal;
    pane.views.retain(|view| {
        let Some(terminal) = view.terminal.upgrade() else {
            return false;
        };
        if let Ok(mut terminal) = terminal.lock() {
            replace_view(&mut terminal, &pane.terminal);
            // Stock captures do not preserve row identity for selections.
            terminal.screen_mut().selection = None;
        }
        (view.wake)();
        true
    });
    Ok(())
}
fn live(shared: &Shared, pane_id: u64, data: &[u8]) -> io::Result<()> {
    let mut model = shared.model.lock().map_err(err)?;
    if let Some(pane) = model.panes.get_mut(&pane_id) {
        let _ = pane.terminal.feed(data);
        pane.views.retain(|view| {
            let Some(terminal) = view.terminal.upgrade() else {
                return false;
            };
            if let Ok(mut terminal) = terminal.lock() {
                feed_view(&mut terminal, data, &view.events);
            }
            (view.wake)();
            true
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use std::time::Instant;
    fn ready(connection: &Connection) -> Graph {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            if let Some(graph) = connection.graph()
                && graph.sessions.len() == 2
                && graph.panes.len() == 3
            {
                return graph;
            }
            assert!(Instant::now() < deadline, "system graph did not converge");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn text(session: &Session, needle: &str) -> bool {
        let terminal = session.terminal().unwrap();
        terminal
            .screen()
            .all_rows()
            .any(|row| terminal.screen().row_text(row).contains(needle))
    }
    fn wait_text(connection: &Connection, session: &Session, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            let text = session.terminal().unwrap().plain_text().replace('\n', "");
            if text.contains(expected) {
                return;
            }
            assert!(Instant::now() < deadline, "missing {expected}: {text:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn stock_mouse_is_targeted(connection: &Connection, first: &Session, host: &str) {
        let program = |first: bool| {
            format!(
                r#"import os,tty
 tty.setraw(0)
 os.write(1,b'\x1b[?1000h\x1b[?1006hMOUSE-READY\r\n')
 def take(n,label):
  data=b''
  while len(data)<n: data+=os.read(0,n-len(data))
  os.write(1,label+b':'+data.hex().encode()+b'\r\n')
 {}
 take(1,b'TEXT')
 os.read(0,1)
 "#,
                if first {
                    "take(9,b'MOUSE')\n take(3,b'REPLY')"
                } else {
                    ""
                }
            )
            .lines()
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
        };
        connection
            .command([
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                "python3",
                "-u",
                "-c",
                &program(true),
            ])
            .unwrap();
        connection
            .command([
                "respawn-pane",
                "-k",
                "-t",
                "%2",
                "python3",
                "-u",
                "-c",
                &program(false),
            ])
            .unwrap();
        let second = Session::attach_tmuxx(connection, 2, Arc::new(|| {})).unwrap();
        wait_text(connection, first, "MOUSE-READY");
        wait_text(connection, &second, "MOUSE-READY");
        connection
            .command(["set-option", "-w", "-t", "one:0", "synchronize-panes", "on"])
            .unwrap();
        let mouse = first.terminal().unwrap().encode_mouse(
            rustty_vt::MouseEvent {
                action: rustty_vt::MouseAction::Press,
                button: Some(rustty_vt::MouseButton::Left),
                x: 20.0,
                y: 10.0,
                modifiers: Default::default(),
            },
            rustty_vt::MouseEncodeOptions {
                screen_size: [800.0, 240.0],
                cell_size: [10, 10],
                padding: [0.0; 4],
                any_button_pressed: true,
                last_cell: None,
            },
        );
        assert_eq!(mouse, b"\x1b[<0;3;2M");
        assert_eq!(
            first
                .write_input(&crate::session::Input::Mouse(vec![0]))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        first
            .write_input(&crate::session::Input::Mouse(mouse))
            .unwrap();
        wait_text(connection, first, "MOUSE:1b5b3c303b333b324d");
        first
            .write_input(&crate::session::Input::Reply(b"r\n;".to_vec()))
            .unwrap();
        wait_text(connection, first, "REPLY:720a3b");
        first.write(b"z").unwrap();
        wait_text(connection, first, "TEXT:7a");
        wait_text(connection, &second, "TEXT:7a");
        connection.command(["kill-pane", "-t", "%2"]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while connection
            .graph()
            .is_none_or(|g| g.panes.iter().any(|p| p.id == 2))
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        second
            .write_input(&crate::session::Input::Reply(b"x".to_vec()))
            .unwrap();
        loop {
            let errors = connection.errors();
            if !errors.is_empty() {
                assert!(
                    errors.iter().all(|error| error.contains("can't find pane")),
                    "{errors:?}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "dead-pane input did not report failure"
            );
            thread::sleep(Duration::from_millis(5));
        }
        // Keep the connection alive until the failed paste's cleanup is confirmed.
        loop {
            let output = Command::new("ssh")
                .args([
                    "-T",
                    "--",
                    host,
                    "exec tmux list-buffers -F '#{buffer_name}'",
                ])
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
            if !String::from_utf8_lossy(&output.stdout).contains("__rustty_input_") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "failed paste left a temporary buffer"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn stock_clipboard_reply_stays_with_c(connection: &Connection, session: &Session) {
        let get_clipboard = std::env::var_os("RUSTTY_TEST_GET_CLIPBOARD").is_some();
        connection
            .command(["set-option", "-s", "set-clipboard", "on"])
            .unwrap();
        if get_clipboard {
            connection
                .command(["set-option", "-s", "get-clipboard", "buffer"])
                .unwrap();
        }
        connection.command(["set-buffer", "clipboard"]).unwrap();
        let script = "import os,tty,re,select\ntty.setraw(0)\nos.write(1,b'CLIPBOARD-READY\\r\\n')\nos.read(0,1)\nos.write(1,b'\\x1b]52;c;?\\x07\\x1b[c')\ndata=b''\nwhile not re.search(rb'\\x1b\\[\\?[0-9;]+c$',data): data+=os.read(0,1)\nreply=re.sub(rb'\\x1b\\[\\?[0-9;]+c$',b'',data)\nif select.select([0],[],[],0.2)[0]: reply+=os.read(0,65536)\nos.write(1,b'CLIPBOARD:'+reply.hex().encode()+b':END\\r\\n')\nos.read(0,1)";
        connection
            .command([
                "respawn-pane",
                "-k",
                "-t",
                "%0",
                "python3",
                "-u",
                "-c",
                script,
            ])
            .unwrap();
        wait_text(connection, session, "CLIPBOARD-READY");
        session.events().for_each(drop);
        session.write(b"q").unwrap();
        wait_text(
            connection,
            session,
            if get_clipboard {
                "CLIPBOARD:1b5d35323b633b5932787063474a7659584a6b07:END"
            } else {
                "CLIPBOARD::END"
            },
        );
        assert!(!session.events().any(|event| matches!(
            event,
            crate::session::SessionEvent::Effect(
                Effect::ClipboardRead(_) | Effect::ClipboardWrite(_)
            )
        )));
        connection.command(["delete-buffer"]).unwrap();
    }
    #[test]
    #[ignore = "requires isolated SSH alias and remote tmux fixture"]
    fn stock_ssh_views_share_live_panes_and_survive_connection_restart() {
        let host = std::env::var("RUSTTY_TEST_SSH_HOST").expect("set RUSTTY_TEST_SSH_HOST");
        let connection = Connection::system_ssh(&host, Arc::new(|| {})).unwrap();
        let graph = ready(&connection);
        assert_eq!(graph.windows.len(), 2);
        assert_eq!(
            graph
                .sessions
                .iter()
                .map(|session| session.windows.len())
                .sum::<usize>(),
            3
        );
        let first = Session::attach_tmuxx(&connection, 0, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, 0, Arc::new(|| {})).unwrap();
        {
            let mut terminal = first.terminal().unwrap();
            let row = terminal.screen().row(0).id;
            terminal.screen_mut().selection = Some(rustty_vt::Selection {
                start: rustty_vt::GridPoint { row, col: 0 },
                end: rustty_vt::GridPoint { row, col: 1 },
                rectangular: false,
            });
        }
        assert!(second.terminal().unwrap().screen().selection.is_none());
        first.write(b"printf 'PRODUCT:%s\\n' 'OK'\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !text(&second, "PRODUCT:OK") {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            assert!(
                Instant::now() < deadline,
                "secondary view did not receive remote input/output"
            );
            thread::sleep(Duration::from_millis(5));
        }
        connection
            .command(["split-window", "-d", "-t", "one:0", "/bin/sh"])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            if connection
                .graph()
                .is_some_and(|graph| graph.panes.len() == 4)
            {
                break;
            }
            assert!(Instant::now() < deadline, "new remote pane did not appear");
            thread::sleep(Duration::from_millis(5));
        }
        let added = Session::attach_tmuxx(&connection, 3, Arc::new(|| {})).unwrap();
        added.write(b"printf 'ADDED:%s\\n' 'OK'\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !text(&added, "ADDED:OK") {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            assert!(
                Instant::now() < deadline,
                "new remote pane output did not arrive"
            );
            thread::sleep(Duration::from_millis(5));
        }
        connection.command(["kill-pane", "-t", "%3"]).unwrap();
        ready(&connection);
        drop(added);
        drop(first);
        drop(second);
        drop(connection);
        let reconnected = Connection::system_ssh(&host, Arc::new(|| {})).unwrap();
        ready(&reconnected);
        let restored = Session::attach_tmuxx(&reconnected, 0, Arc::new(|| {})).unwrap();
        assert!(text(&restored, "PRODUCT:OK"));
        reconnected
            .command(["link-window", "-s", "one:0", "-t", "one:4"])
            .unwrap();
        reconnected
            .command(["select-window", "-t", "one:4"])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let errors = reconnected.errors();
            assert!(errors.is_empty(), "{errors:?}");
            if reconnected.graph().is_some_and(|g| {
                g.sessions.iter().any(|s| {
                    s.name == "one"
                        && s.windows.get(&0) == s.windows.get(&4)
                        && s.windows.contains_key(&4)
                        && s.active == 4
                })
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "same-session duplicate winlinks did not converge"
            );
            thread::sleep(Duration::from_millis(5));
        }
        stock_clipboard_reply_stays_with_c(&reconnected, &restored);
        if std::env::var_os("RUSTTY_TEST_FLOATING").is_some() {
            let modal = std::env::var_os("RUSTTY_TEST_MODAL").is_some();
            let before = reconnected
                .graph()
                .unwrap()
                .panes
                .iter()
                .map(|pane| pane.id)
                .collect::<std::collections::BTreeSet<_>>();
            let mut args = vec![
                "new-pane", "-t", "%0", "-x", "24", "-y", "8", "-X", "-3", "-Y", "2",
            ];
            if modal {
                args.push("-O");
            }
            args.extend(["sleep", "30"]);
            reconnected.command(args).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let pane = loop {
                let errors = reconnected.errors();
                assert!(errors.is_empty(), "{errors:?}");
                if let Some(graph) = reconnected.graph()
                    && let Some(pane) = graph.panes.iter().find(|pane| !before.contains(&pane.id))
                {
                    assert_eq!(pane.modal, modal);
                    let window = graph
                        .windows
                        .iter()
                        .find(|window| window.id == pane.window)
                        .unwrap();
                    let layout = Layout::parse(&window.layout)
                        .unwrap_or_else(|error| panic!("{error}: {}", window.layout));
                    let cell = layout.find(pane.id as u32).unwrap();
                    assert!(cell.geometry.x < 0);
                    assert!(matches!(&cell.kind, Kind::Pane(pane) if pane.z == Some(0)));
                    break pane.id;
                }
                assert!(
                    Instant::now() < deadline,
                    "signed floating/modal pane did not appear in stock discovery"
                );
                thread::sleep(Duration::from_millis(5));
            };
            reconnected
                .command(["kill-pane".to_owned(), "-t".to_owned(), format!("%{pane}")])
                .unwrap();
            ready(&reconnected);
        }
        stock_mouse_is_targeted(&reconnected, &restored, &host);
        reconnected
            .command(["run-shell", "printf '%s\\n' deferred-rustty-server-message"])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let messages = reconnected.errors();
            if messages
                .iter()
                .any(|message| message == "deferred-rustty-server-message")
            {
                break;
            }
            assert!(messages.is_empty(), "unexpected server text: {messages:?}");
            assert!(
                Instant::now() < deadline,
                "deferred server text was dropped"
            );
            thread::sleep(Duration::from_millis(5));
        }
        drop(restored);
        drop(reconnected);
    }
}
