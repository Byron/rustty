//! Per-client command queues. Waiting never holds the process owner loop.
use super::*;
use std::collections::VecDeque;
#[cfg(unix)]
use std::process::{Command as ProcessCommand, Stdio};
use tmuxx_command::{Args, Command};

const COMMAND_QUANTUM: usize = 64;
const MAX_PENDING_REQUESTS: usize = 64;
const MAX_JOBS: usize = 128;
const MAX_CHANNELS: usize = 4096;
const MAX_COMMANDS: usize = 16384;

#[derive(Default)]
pub(super) struct Scheduler {
    queues: BTreeMap<u64, Queue>,
    files: BTreeMap<u64, (u64, u64, PendingFile)>,
    stdin_clients: BTreeSet<u64>,
    stdin_consumed: BTreeSet<u64>,
    stdin_files: BTreeMap<u64, FileInput>,
    stdin_errors: BTreeMap<u64, (u64, String)>,
    ready: VecDeque<u64>,
    channels: BTreeMap<Vec<u8>, Channel>,
    event_waiters: BTreeMap<u64, EventWaiter>,
    jobs: BTreeMap<u64, Job>,
    next_job: u64,
    next_internal: u64,
    internal: BTreeMap<u64, Receiver<ServerMessage>>,
    startup: Option<u64>,
}
#[derive(Default)]
struct Queue {
    pending: VecDeque<Batch>,
    blocked: bool,
    bytes: usize,
}

struct FileInput {
    request_id: u64,
    bytes: Vec<u8>,
    error: Option<String>,
}
fn command_size<'a>(commands: impl Iterator<Item = &'a Command>) -> usize {
    commands
        .map(|command| {
            command
                .args
                .iter()
                .map(|arg| arg.len().saturating_add(1))
                .sum::<usize>()
        })
        .sum()
}
struct Batch {
    request_id: u64,
    acknowledge: bool,
    commands: VecDeque<Queued>,
    next_group: u64,
    stdin: Option<Vec<u8>>,
    output: Vec<u8>,
    error: Option<String>,
    status: Option<i32>,
    subscribe: bool,
    control: bool,
    control_guards: Option<u64>,
    started: bool,
    bytes: usize,
}
struct Queued {
    command: Command,
    depth: usize,
    group: u64,
    context: Option<ClientContext>,
}
impl Batch {
    fn fail(&mut self, group: u64, error: String) {
        self.error.get_or_insert(error);
        self.commands.retain(|item| item.group != group);
    }
    fn insert(
        &mut self,
        commands: Vec<Command>,
        depth: usize,
        group: u64,
        context: Option<ClientContext>,
    ) {
        if self.commands.len().saturating_add(commands.len()) > MAX_COMMANDS
            || command_size(self.commands.iter().map(|item| &item.command))
                .saturating_add(command_size(commands.iter()))
                > MAX_REQUEST_BYTES
        {
            self.fail(group, "expanded command queue exceeds limit".into());
            return;
        }
        for command in commands.into_iter().rev() {
            self.commands.push_front(Queued {
                command,
                depth,
                group,
                context: context.clone(),
            });
        }
    }
    fn insert_hooks(&mut self, hooks: Vec<super::hooks::Invocation>, depth: usize) {
        for hook in hooks.into_iter().rev() {
            let group = self.next_group;
            self.next_group += 1;
            self.insert(hook.commands, depth, group, Some(hook.context));
        }
    }
    fn insert_source(&mut self, source: SourceFiles) {
        self.output.extend(source.trace);
        if let Some(error) = source.error {
            self.error.get_or_insert(error);
        }
        for commands in source.groups.into_iter().rev() {
            let group = self.next_group;
            self.next_group += 1;
            self.insert(
                commands,
                source.depth + 1,
                group,
                Some(source.context.clone()),
            );
        }
    }
}
#[derive(Default)]
struct Channel {
    woken: bool,
    locked: bool,
    waiters: VecDeque<u64>,
    lockers: VecDeque<u64>,
}
struct EventWaiter {
    name: Vec<u8>,
    filter: Option<Vec<u8>>,
    verbose: bool,
    group: u64,
}
enum CommandsArgument {
    Source(Vec<u8>),
    Parsed(Vec<Command>),
}
impl CommandsArgument {
    fn get(command: &Command, args: &Args, index: usize) -> Option<Self> {
        args.rest.get(index).map(|source| {
            command.argument_block(args, index).map_or_else(
                || Self::Source(source.clone()),
                |block| Self::Parsed(block.to_vec()),
            )
        })
    }
}
struct Job {
    client: u64,
    context: ClientContext,
    background: bool,
    command: Vec<u8>,
    then: Option<CommandsArgument>,
    otherwise: Option<CommandsArgument>,
    conditional: bool,
    commands_only: bool,
    depth: usize,
    group: u64,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(super) struct JobResult {
    id: u64,
    output: Vec<u8>,
    status: i32,
    signal: Option<i32>,
    error: Option<String>,
}
struct PreparedSource {
    groups: Vec<(usize, Vec<Command>)>,
    error: Option<String>,
}
struct SourceFiles {
    paths: VecDeque<PathBuf>,
    context: ClientContext,
    depth: usize,
    parse_only: bool,
    verbose: bool,
    groups: Vec<Vec<Command>>,
    diagnostics: Vec<u8>,
    trace: Vec<u8>,
    error: Option<String>,
    control_output: Vec<u8>,
    deferred_output: Vec<u8>,
}
impl SourceFiles {
    fn error(&mut self, error: String) {
        self.deferred_output
            .extend(format!("{error}\n").into_bytes());
        if let Some(previous) = &mut self.error {
            previous.push('\n');
            previous.push_str(&error);
        } else {
            self.error = Some(error);
        }
    }
}
enum PendingFile {
    Buffer(Effect),
    Source(Box<SourceFiles>),
}
impl PendingFile {
    fn message(&self, request_id: u64) -> ServerMessage {
        let (path, data, append) = match self {
            Self::Buffer(Effect::LoadBuffer { path, .. }) => (path.clone(), None, false),
            Self::Buffer(Effect::SaveBuffer { path, data, append }) => {
                (path.clone(), Some(data.clone()), *append)
            }
            Self::Source(_) => (b"-".to_vec(), None, false),
            _ => unreachable!(),
        };
        ServerMessage::File {
            request_id,
            path,
            data,
            append,
        }
    }
}

enum Step {
    Done(Outcome),
    File(PendingFile),
    NativeFile(Effect),
    InsertContext(Vec<Command>, ClientContext),
    SourceResult(Vec<u8>, bool),
    InsertSource(Box<SourceFiles>),
    Hooks(Outcome, Vec<super::hooks::Invocation>),
    Wait,
}

impl Scheduler {
    pub(super) fn enable_stdin_files(&mut self, id: u64) {
        self.stdin_clients.insert(id);
    }
    pub(super) fn job_summaries(&self) -> impl Iterator<Item = (u64, &[u8], &'static str)> {
        self.jobs.iter().map(|(id, job)| {
            (
                *id,
                job.command.as_slice(),
                if job.commands_only {
                    "command"
                } else {
                    "shell"
                },
            )
        })
    }
    pub(super) fn next_job_id(&mut self) -> u64 {
        let id = self.next_job;
        self.next_job += 1;
        id
    }
    pub(super) fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }
    pub(super) fn flush_waiters(&mut self) {
        let mut ids = std::mem::take(&mut self.event_waiters)
            .into_keys()
            .collect::<Vec<_>>();
        for channel in std::mem::take(&mut self.channels).into_values() {
            ids.extend(channel.waiters);
            ids.extend(channel.lockers);
        }
        for id in ids {
            self.unblock(id);
        }
    }
    pub(super) fn shutdown(&mut self) {
        for job in self.jobs.values() {
            job.stop.store(true, Ordering::Release);
        }
        self.jobs.clear();
    }
    fn ready(&mut self, id: u64) {
        if self.startup.is_none_or(|startup| startup == id) && !self.ready.contains(&id) {
            self.ready.push_back(id);
        }
    }
    fn unblock(&mut self, id: u64) {
        if let Some(queue) = self.queues.get_mut(&id) {
            queue.blocked = false;
            self.ready(id);
        }
    }
    fn clean_channel(&mut self, name: &[u8]) {
        if self.channels.get(name).is_some_and(|channel| {
            !channel.locked
                && !channel.woken
                && channel.waiters.is_empty()
                && channel.lockers.is_empty()
        }) {
            self.channels.remove(name);
        }
    }
    pub(super) fn lost(&mut self, id: u64) {
        self.queues.remove(&id);
        self.files.remove(&id);
        self.stdin_clients.remove(&id);
        self.stdin_consumed.remove(&id);
        self.stdin_files.remove(&id);
        self.stdin_errors.remove(&id);
        self.ready.retain(|other| *other != id);
        self.internal.remove(&id);
        self.event_waiters.remove(&id);
        for channel in self.channels.values_mut() {
            channel.waiters.retain(|other| *other != id);
            channel.lockers.retain(|other| *other != id);
        }
        self.channels.retain(|_, channel| {
            channel.locked
                || channel.woken
                || !channel.waiters.is_empty()
                || !channel.lockers.is_empty()
        });
        // Started jobs remain owner-owned; their late output cannot revive a client.
    }
}

impl Host {
    pub(super) fn scheduler_has_file_stdin(&self, id: u64) -> bool {
        self.scheduler.stdin_files.contains_key(&id)
    }

    fn send_file_request(&mut self, id: u64, request_id: u64, pending: &PendingFile) {
        if self.scheduler.stdin_clients.contains(&id) {
            if !self.scheduler.stdin_consumed.insert(id) {
                // Complete the asynchronous file callback after the active queue
                // has been restored, retaining C's continuation-after-EBADF rule.
                self.scheduler
                    .stdin_errors
                    .insert(id, (request_id, "Bad file descriptor: -".into()));
                return;
            }
            self.scheduler.stdin_files.insert(
                id,
                FileInput {
                    request_id,
                    bytes: Vec::new(),
                    error: None,
                },
            );
        }
        self.send(id, pending.message(request_id));
    }

    pub(super) fn file_stdin_chunk(
        &mut self,
        id: u64,
        request_id: u64,
        data: Result<Option<Vec<u8>>, String>,
    ) -> Option<Result<(), String>> {
        let stream = self.scheduler.stdin_files.get_mut(&id)?;
        if stream.request_id != request_id {
            return Some(Err("no pending stdin request".into()));
        }
        match data {
            Ok(Some(bytes)) if bytes.len() > 65536 => {
                return Some(Err("stdin chunk exceeds limit".into()));
            }
            Ok(Some(bytes)) => {
                if stream.error.is_none() {
                    if stream.bytes.len().saturating_add(bytes.len()) > MAX_REQUEST_BYTES {
                        stream.bytes.clear();
                        stream.error = Some("file input exceeds limit: -".into());
                    } else {
                        stream.bytes.extend(bytes);
                    }
                }
                // After overflow keep draining acknowledged chunks until EOF.
                // This bounds memory and lets the producer close its pipe.
            }
            Ok(None) | Err(_) => {
                let stream = self.scheduler.stdin_files.remove(&id).unwrap();
                let result = match data {
                    Err(error) => Err(error),
                    _ => stream.error.map_or(Ok(stream.bytes), Err),
                };
                self.file_complete(id, request_id, result);
            }
        }
        Some(Ok(()))
    }

    pub(super) fn file_complete(
        &mut self,
        id: u64,
        request_id: u64,
        result: Result<Vec<u8>, String>,
    ) {
        if self
            .scheduler
            .files
            .get(&id)
            .is_none_or(|(request, _, _)| *request != request_id)
        {
            return;
        }
        let (_, group, pending) = self.scheduler.files.remove(&id).unwrap();
        let result = match pending {
            PendingFile::Source(mut source) => {
                let result = match result {
                    Ok(data) if data.len() <= MAX_REQUEST_BYTES => {
                        self.collect_source(&mut source, &data, "-")
                    }
                    Ok(_) => Err("configuration exceeds size limit".into()),
                    Err(error) if error.starts_with("file input exceeds limit:") => Err(error),
                    Err(error) => {
                        source.error(error);
                        Ok(())
                    }
                }
                .and_then(|()| self.continue_source(id, source));
                match result {
                    Ok(Step::File(pending)) => {
                        self.send_file_request(id, request_id, &pending);
                        self.scheduler
                            .files
                            .insert(id, (request_id, group, pending));
                        return;
                    }
                    Ok(Step::InsertSource(source)) => {
                        let mut deferred = None;
                        if let Some(batch) = self
                            .scheduler
                            .queues
                            .get_mut(&id)
                            .and_then(|queue| queue.pending.front_mut())
                        {
                            if batch.control_guards.is_some() {
                                deferred = Some(source.deferred_output.clone());
                            }
                            batch.insert_source(*source);
                        }
                        if let Some(data) = deferred {
                            self.control_unframed_output(id, data);
                        }
                        Ok(())
                    }
                    Err(error) => Err(error),
                    _ => unreachable!(),
                }
            }
            PendingFile::Buffer(effect) => result.and_then(|data| {
                if data.len() > MAX_REQUEST_BYTES {
                    return Err("buffer exceeds input size limit".into());
                }
                if let Effect::LoadBuffer {
                    name, clipboard, ..
                } = effect
                {
                    if data.is_empty() {
                        return Ok(());
                    }
                    let clipboard_data = clipboard.as_ref().map(|_| data.clone());
                    let (name, deleted) = self.engine.store_buffer(name, data)?.unwrap();
                    if let Some((client, pane)) = clipboard {
                        self.client_clipboard(&client, pane, clipboard_data.unwrap())?;
                    }
                    for name in deleted {
                        self.broadcast(ServerMessage::Control {
                            data: format!("%paste-buffer-deleted {name}").into_bytes(),
                        });
                    }
                    self.broadcast(ServerMessage::Control {
                        data: format!("%paste-buffer-changed {name}").into_bytes(),
                    });
                }
                Ok(())
            }),
        };
        if let Err(error) = result {
            let mut wire = false;
            if let Some(batch) = self
                .scheduler
                .queues
                .get_mut(&id)
                .and_then(|queue| queue.pending.front_mut())
            {
                wire = batch.control_guards.is_some();
                if error.starts_with("file input exceeds limit:")
                    || matches!(
                        error.as_str(),
                        "buffer exceeds input size limit" | "configuration exceeds size limit"
                    )
                {
                    // Resource-bound rejection intentionally aborts this group.
                    batch.fail(group, error.clone());
                } else {
                    // C's file callback reports an error after WAIT has already
                    // closed the guard; later commands in the group still run.
                    batch.error.get_or_insert(error.clone());
                }
            }
            if wire {
                self.control_unframed_output(id, format!("{error}\n").into_bytes());
            }
        }
        self.scheduler.unblock(id);
    }
    pub(super) fn pane_wait_complete(&mut self, pane: u64, status: i32) {
        let done = self
            .pane_waiters
            .iter()
            .filter(|(_, (waiting, _))| *waiting == pane)
            .map(|(id, (_, attached))| (*id, *attached))
            .collect::<Vec<_>>();
        for (id, attached) in done {
            self.pane_waiters.remove(&id);
            if !attached
                && let Some(batch) = self
                    .scheduler
                    .queues
                    .get_mut(&id)
                    .and_then(|queue| queue.pending.front_mut())
            {
                batch.status = Some(status);
            }
            self.scheduler.unblock(id);
        }
    }
    pub(super) fn stdin_complete(&mut self, id: u64, error: Option<String>) {
        let Some(stream) = self.stdin_streams.remove(&id) else {
            return;
        };
        if let Some(error) = error
            && let Some(batch) = self
                .scheduler
                .queues
                .get_mut(&id)
                .and_then(|queue| queue.pending.front_mut())
        {
            batch.fail(stream.group, error);
        }
        self.scheduler.unblock(id);
    }
    pub(super) fn request(&mut self, id: u64, request: Request) {
        self.request_parsed(id, request, None, None);
    }

    pub(super) fn request_parsed(
        &mut self,
        id: u64,
        request: Request,
        prepared: Option<Result<Vec<Command>, String>>,
        control_guards: Option<u64>,
    ) {
        if !self.clients.contains_key(&id)
            || self
                .control
                .clients
                .get(&id)
                .is_some_and(|client| client.closing.is_some())
        {
            return;
        }
        if control_guards.is_some() {
            self.control.clients.entry(id).or_default().wire = true;
        }
        // Input chunks bypass the command queue that is waiting for their EOF.
        // Ownership is still checked against this connection and original request.
        if request
            .argv
            .first()
            .is_some_and(|arg| arg == b"tmuxx-stdin")
        {
            let result = if request.argv.len() != 2 {
                Err("usage: tmuxx-stdin request-id".into())
            } else if let Some(original) = std::str::from_utf8(&request.argv[1])
                .ok()
                .and_then(|id| id.parse().ok())
            {
                self.stdin_chunk(id, original, Ok(request.stdin))
            } else {
                Err("invalid stdin request id".into())
            };
            self.send(
                id,
                ServerMessage::Reply(Response {
                    request_id: request.request_id,
                    status: None,
                    output: Vec::new(),
                    error: result.err(),
                }),
            );
            return;
        }
        let bytes = request.argv.iter().map(Vec::len).sum::<usize>()
            + request.stdin.as_ref().map_or(0, Vec::len);
        let parsed = match prepared {
            Some(commands) => commands,
            None => tmuxx_command::parse_argv(&request.argv)
                .map_err(|error| error.to_string())
                .and_then(|commands| self.prepare_aliases(commands)),
        };
        let (commands, error) = match parsed {
            Ok(commands) => (
                commands
                    .into_iter()
                    .map(|command| Queued {
                        command,
                        depth: 0,
                        group: 0,
                        context: None,
                    })
                    .collect(),
                None,
            ),
            Err(error) => (VecDeque::new(), Some(error.to_string())),
        };
        let queue = self.scheduler.queues.entry(id).or_default();
        if request.argv.len() > MAX_COMMANDS
            || queue.pending.len() >= MAX_PENDING_REQUESTS
            || queue.bytes.saturating_add(bytes) > MAX_REQUEST_BYTES
        {
            self.send(
                id,
                ServerMessage::Exit {
                    reason: "pending command queue exceeds limit".into(),
                },
            );
            self.disconnect(id);
            return;
        }
        queue.bytes += bytes;
        queue.pending.push_back(Batch {
            next_group: 1,
            request_id: request.request_id,
            acknowledge: true,
            commands,
            stdin: request.stdin,
            output: Vec::new(),
            error,
            status: None,
            subscribe: request.subscribe,
            control: request.control,
            control_guards,
            started: false,
            bytes,
        });
        if !queue.blocked {
            self.scheduler.ready(id);
        }
    }

    pub(super) fn disconnect(&mut self, id: u64) {
        self.snapshots_pending.remove(&id);
        self.snapshots_requested.remove(&id);
        self.pane_waiters.remove(&id);
        self.stdin_streams.remove(&id);
        self.file_clients.remove(&id);
        if let Some(client) = self.clients.get(&id).cloned() {
            self.disconnect_format_jobs(&client.context.client_name);
            self.client_closed(id, &client.context);
        }
        self.control.clients.remove(&id);
        self.clients.remove(&id);
        self.sync_control_colors();
        self.scheduler.lost(id);
        self.disconnect_host(id);
        self.recalculate_sizes();
        self.check_unattached_sessions();
    }

    pub(super) fn internal_queue(
        &mut self,
        context: ClientContext,
        commands: Vec<Command>,
        depth: usize,
        startup: bool,
    ) {
        let id = u64::MAX - self.scheduler.next_internal;
        self.scheduler.next_internal += 1;
        let (tx, rx) = mpsc::sync_channel(16);
        self.clients.insert(
            id,
            Client {
                tx,
                context,
                subscribed: false,
                control: false,
                delivery_bytes: Arc::new(super::AtomicUsize::new(0)),
            },
        );
        self.scheduler.internal.insert(id, rx);
        if startup {
            self.scheduler.startup = Some(id);
        }
        let bytes = command_size(commands.iter());
        let commands = commands
            .into_iter()
            .map(|command| Queued {
                command,
                depth,
                group: 0,
                context: None,
            })
            .collect();
        let batch = Batch {
            request_id: 0,
            acknowledge: true,
            commands,
            next_group: 1,
            stdin: None,
            output: Vec::new(),
            error: None,
            status: None,
            subscribe: false,
            control: false,
            control_guards: None,
            started: false,
            bytes,
        };
        self.scheduler.queues.insert(
            id,
            Queue {
                pending: VecDeque::from([batch]),
                bytes,
                blocked: false,
            },
        );
        self.scheduler.ready(id);
    }

    pub(super) fn start_configuration(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let commands = paths
            .iter()
            .map(|path| Command {
                blocks: Default::default(),
                args: vec![
                    b"source-file".to_vec(),
                    path.as_os_str().as_encoded_bytes().to_vec(),
                ],
            })
            .collect();
        self.internal_queue(ClientContext::default(), commands, 0, true);
    }

    pub(super) fn scheduled_work(&mut self) -> bool {
        let errors = std::mem::take(&mut self.scheduler.stdin_errors);
        let completed_files = !errors.is_empty();
        for (id, (request_id, error)) in errors {
            self.file_complete(id, request_id, Err(error));
        }
        let Some(id) = self.scheduler.ready.pop_front() else {
            return completed_files;
        };
        self.drive(id);
        true
    }

    fn drive(&mut self, id: u64) {
        let Some(mut client) = self.clients.get(&id).cloned() else {
            self.scheduler.lost(id);
            return;
        };
        let Some(mut queue) = self.scheduler.queues.remove(&id) else {
            self.clients.insert(id, client);
            return;
        };
        let mut events = Vec::new();
        let mut client_events = Vec::new();
        let mut changed = false;
        let mut snapshots = false;
        let mut replies = Vec::new();
        let mut detach = false;
        let mut detach_hangup = false;
        let mut budget = COMMAND_QUANTUM;
        while !queue.blocked && budget != 0 && !detach {
            if !self.clients.contains_key(&id) {
                break;
            }
            let Some(batch) = queue.pending.front_mut() else {
                break;
            };
            if !batch.started {
                client.subscribed |= batch.subscribe;
                client.control = batch.control;
                client.context.control = client.subscribed || client.control;
                client.context.native_view = client.subscribed && !client.control;
                if client.control {
                    self.initialize_control(id, &mut client.context);
                }
                if client.context.control && !self.scheduler.internal.contains_key(&id) {
                    self.client_opened(id, &client.context);
                }
                batch.started = true;
                if let Some(flags) = batch.control_guards
                    && batch.commands.is_empty()
                    && let Some(error) = &batch.error
                {
                    self.clients.insert(id, client.clone());
                    self.control_command_result(id, flags, Vec::new(), Some(error));
                }
            }
            if let Some(item) = batch.commands.pop_front() {
                budget -= 1;
                self.clients.insert(id, client.clone());
                self.sync_control_colors();
                self.request_stdin = batch.stdin.take();
                self.stdin_target = None;
                self.wait_pane_target = None;
                let mut context = item
                    .context
                    .clone()
                    .unwrap_or_else(|| client.context.clone());
                if !self.scheduler.internal.contains_key(&id) {
                    context.attached = self.hooks.attached.contains(&id);
                }
                let previous_context = context.clone();
                let policies = self.session_policies();
                let before = self.command_hook_snapshot(&item.command, &context);
                self.log_command(&item.command);
                let result =
                    self.scheduled_command(id, &item.command, &mut context, item.depth, item.group);
                if let Err(error) = &result {
                    self.log_message(b"error", error.as_bytes());
                }
                self.trim_messages();
                detach |= self.reconcile_destroyed_sessions(policies, Some((id, &mut context)));
                let mut hooks = self.command_hooks(&item.command, &context, before, result.is_ok());
                let recalculate_now = result.is_ok() && self.command_sizes(&item.command);
                if result.is_ok()
                    && !item.command.args.first().is_some_and(|name| {
                        matches!(name.as_slice(), b"switch-client" | b"switchc")
                    })
                {
                    self.client_command_hooks(id, &item.command, &previous_context, &context);
                }
                let result = match result {
                    Ok(Step::Hooks(out, immediate)) => {
                        hooks.splice(0..0, immediate);
                        Ok(Step::Done(out))
                    }
                    other => other,
                };
                if item.context.is_none() {
                    client.context = context.clone();
                }
                if let Some(stored) = self.clients.get_mut(&id) {
                    *stored = client.clone();
                }
                if recalculate_now {
                    self.recalculate_sizes();
                }
                batch.stdin = self.request_stdin.take();
                let mut waiting = false;
                let mut late_output = Vec::new();
                let mut command_error = result.as_ref().err().cloned();
                let callback = item
                    .command
                    .args
                    .first()
                    .is_some_and(|name| name == b"tmuxx-source-diagnostics");
                match result {
                    Ok(Step::Done(mut out)) | Ok(Step::Hooks(mut out, _)) => {
                        self.deliver_session_change(id, &mut out);
                        batch.output.extend(out.output);
                        for notification in out.notifications {
                            events.extend(self.prepare_notification(notification));
                        }
                        client_events.extend(out.client_notifications);
                        changed |= out.changed;
                        detach |= out.detach;
                        detach_hangup |= out.detach_hangup;
                        if out.changed
                            && let Err(error) = self.refresh_view_states()
                        {
                            batch.fail(item.group, error);
                        }
                        if batch.output.len() > MAX_REQUEST_BYTES {
                            batch.fail(item.group, "command output exceeds limit".into());
                            batch.output.clear();
                        }
                        if let Some(pane) = self.stdin_target.take()
                            && !context.control
                            && batch.error.is_none()
                        {
                            self.stdin_streams.insert(
                                id,
                                super::process::StdinStream {
                                    pane,
                                    request_id: batch.request_id,
                                    group: item.group,
                                },
                            );
                            queue.blocked = true;
                            waiting = true;
                            self.send(
                                id,
                                ServerMessage::Stdin {
                                    request_id: batch.request_id,
                                    pane_id: pane,
                                },
                            );
                        }
                        if let Some(pane) = self.wait_pane_target.take()
                            && batch.error.is_none()
                        {
                            self.pane_waiters.insert(id, (pane, context.control));
                            queue.blocked = true;
                            waiting = true;
                        }
                    }
                    Ok(Step::SourceResult(output, failed)) => {
                        if client.control {
                            let state = self.control.clients.entry(id).or_default();
                            for line in output
                                .split(|byte| *byte == b'\n')
                                .filter(|line| !line.is_empty())
                            {
                                let mut data = b"%config-error ".to_vec();
                                data.extend_from_slice(line);
                                state.after_reply.push(ServerMessage::Control { data });
                            }
                        } else {
                            batch.output.extend(output);
                        }
                        if failed {
                            batch.status = Some(1);
                        }
                    }
                    Ok(Step::InsertSource(mut source)) => {
                        if batch.control_guards.is_some() {
                            batch.output.append(&mut source.control_output);
                            late_output.append(&mut source.deferred_output);
                        }
                        batch.insert_source(*source);
                    }
                    Ok(Step::InsertContext(commands, context)) => {
                        batch.insert(commands, item.depth + 1, item.group, Some(context))
                    }
                    Ok(Step::File(mut pending)) => {
                        if batch.control_guards.is_some()
                            && let PendingFile::Source(source) = &mut pending
                        {
                            batch.output.append(&mut source.control_output);
                            late_output.append(&mut source.deferred_output);
                        }
                        self.send_file_request(id, batch.request_id, &pending);
                        self.scheduler
                            .files
                            .insert(id, (batch.request_id, item.group, pending));
                        queue.blocked = true;
                        waiting = true;
                    }
                    Ok(Step::NativeFile(effect)) => {
                        let pending = PendingFile::Buffer(effect.clone());
                        let request_id = batch.request_id;
                        let tx = self.tx.clone();
                        match thread::Builder::new()
                            .name("tmuxx-native-file".into())
                            .spawn(move || {
                                let result = native_control_file(effect);
                                let _ = tx.send(Event::FileResult {
                                    id,
                                    request_id,
                                    result,
                                });
                            }) {
                            Ok(_) => {
                                self.scheduler
                                    .files
                                    .insert(id, (request_id, item.group, pending));
                                queue.blocked = true;
                                waiting = true;
                            }
                            Err(error) => {
                                command_error = Some(error.to_string());
                                batch.fail(item.group, error.to_string());
                            }
                        }
                    }
                    Ok(Step::Wait) => {
                        queue.blocked = true;
                        waiting = true;
                    }
                    Err(error) => batch.fail(item.group, error),
                }
                batch.insert_hooks(hooks, item.depth + 1);
                if let Some(flags) = batch.control_guards {
                    let output = std::mem::take(&mut batch.output);
                    if callback {
                        self.control_unframed_output(id, output);
                        self.control_after_reply(id);
                    } else {
                        self.control_command_result(
                            id,
                            if context.no_hooks { 0 } else { flags },
                            output,
                            command_error.as_deref(),
                        );
                    }
                    self.control_unframed_output(id, late_output);
                    self.deliver_client_notifications(std::mem::take(&mut client_events));
                    for (target, message) in events.drain(..) {
                        self.send(target, message);
                    }
                }
                if waiting {
                    break;
                }
                if !batch.commands.is_empty() && !detach {
                    continue;
                }
            }
            let batch = queue.pending.pop_front().unwrap();
            queue.bytes -= batch.bytes;
            snapshots |= batch.subscribe;
            if batch.acknowledge {
                replies.push(Response {
                    request_id: batch.request_id,
                    status: batch.status,
                    output: batch.output,
                    error: batch.error,
                });
            }
        }
        let finished = queue.pending.is_empty();
        let blocked = queue.blocked;
        if !self.clients.contains_key(&id) {
            self.scheduler.lost(id);
            return;
        }
        self.scheduler.queues.insert(id, queue);
        if self.clients.contains_key(&id) {
            self.clients.insert(id, client);
        }
        self.sync_control_colors();
        if self.sizes_pending {
            self.recalculate_sizes();
        }
        self.check_unattached_sessions();
        for reply in replies {
            if self.scheduler.internal.contains_key(&id)
                && let Some(error) = &reply.error
            {
                self.broadcast(ServerMessage::Control {
                    data: format!("%config-error {error}").into_bytes(),
                });
            }
            self.send(id, ServerMessage::Reply(reply));
        }
        self.deliver_client_notifications(client_events);
        for (target, message) in events {
            self.send(target, message);
        }
        if snapshots {
            self.snapshot(id);
        } else if changed {
            self.broadcast(ServerMessage::Changed {
                sequence: self.engine.sequence,
            });
        }
        if detach {
            self.detach_client(id, detach_hangup);
        } else if finished && self.scheduler.internal.contains_key(&id) {
            let startup = self.scheduler.startup == Some(id);
            self.disconnect(id);
            if startup {
                self.scheduler.startup = None;
                for id in self.scheduler.queues.keys().copied().collect::<Vec<_>>() {
                    self.scheduler.ready(id);
                }
            }
        } else if !finished && !blocked {
            self.scheduler.ready(id);
        }
    }

    fn deliver_client_notifications(&mut self, notifications: Vec<(String, Vec<u8>)>) {
        for (name, data) in notifications {
            let targets = self
                .clients
                .iter()
                .filter(|(_, client)| client.subscribed && client.context.session.is_some())
                .map(|(id, client)| (*id, client.context.client_name == name))
                .collect::<Vec<_>>();
            for (target, same) in targets {
                let data = if same {
                    data.clone()
                } else {
                    let mut notice = format!("%client-session-changed {name} ").into_bytes();
                    notice.extend_from_slice(&data[b"%session-changed ".len()..]);
                    notice
                };
                self.send(target, ServerMessage::Control { data });
            }
        }
    }

    fn background_control_commands(
        &mut self,
        id: u64,
        commands: Vec<Command>,
        mut context: ClientContext,
        depth: usize,
    ) {
        let bytes = command_size(commands.iter());
        let queue = self.scheduler.queues.entry(id).or_default();
        if commands.len() > MAX_COMMANDS
            || queue.pending.len() >= MAX_PENDING_REQUESTS
            || queue.bytes.saturating_add(bytes) > MAX_REQUEST_BYTES
        {
            self.send(
                id,
                ServerMessage::Exit {
                    reason: "pending command queue exceeds limit".into(),
                },
            );
            self.disconnect(id);
            return;
        }
        context.no_hooks = false;
        let commands = commands
            .into_iter()
            .map(|command| Queued {
                command,
                depth,
                group: 0,
                context: Some(context.clone()),
            })
            .collect();
        queue.pending.push_back(Batch {
            request_id: 0,
            acknowledge: false,
            commands,
            next_group: 1,
            stdin: None,
            output: Vec::new(),
            error: None,
            status: None,
            subscribe: false,
            control: true,
            control_guards: Some(0),
            started: true,
            bytes,
        });
        queue.bytes += bytes;
        if !queue.blocked {
            self.scheduler.ready(id);
        }
    }

    fn scheduled_command(
        &mut self,
        id: u64,
        command: &Command,
        context: &mut ClientContext,
        depth: usize,
        group: u64,
    ) -> Result<Step, String> {
        let previous = context.command;
        context.command = command
            .args
            .first()
            .and_then(|name| tmuxx_command::registry::find(name).ok())
            .map(|entry| entry.name);
        let result = self.scheduled_command_inner(id, command, context, depth, group);
        context.command = previous;
        result
    }

    fn scheduled_command_inner(
        &mut self,
        id: u64,
        command: &Command,
        context: &mut ClientContext,
        depth: usize,
        group: u64,
    ) -> Result<Step, String> {
        if depth > 64 {
            return Err("command nesting limit exceeded".into());
        }
        let name = command.args.first().map(Vec::as_slice).unwrap_or_default();
        if tmuxx_command::registry::find(name).is_ok() {
            tmuxx_command::registry::validate_server(command)?;
        }
        self.refresh_format_clients();
        if !context.no_hooks
            && context
                .client_flags
                .split(',')
                .any(|flag| flag == "read-only")
            && (!super::hooks::command_readonly(name) || matches!(name, b"send-keys" | b"send"))
        {
            return Err("client is read-only".into());
        }
        if matches!(name, b"set-hook" | b"show-hooks") {
            return self
                .hook_command(command, context)
                .map(|(out, hooks)| Step::Hooks(out, hooks));
        }
        if matches!(name, b"new-session" | b"new") {
            return self
                .new_session_client(id, command, context)
                .map(Step::Done);
        }
        if matches!(
            name,
            b"attach-session"
                | b"attach"
                | b"switch-client"
                | b"switchc"
                | b"detach-client"
                | b"detach"
        ) {
            return self.client_command(id, command, context).map(Step::Done);
        }
        if matches!(name, b"refresh-client" | b"refresh") {
            return self.refresh_control(id, command, context).map(Step::Done);
        }
        if matches!(name, b"show-messages" | b"showmsgs") {
            return self.show_messages(command).map(Step::Done);
        }
        if name == b"tmuxx-cell-size" {
            if command.args.len() != 3 {
                return Err("expected cell width and height".into());
            }
            let values = command.args[1..]
                .iter()
                .map(|value| {
                    std::str::from_utf8(value)
                        .ok()
                        .and_then(|value| value.parse::<u16>().ok())
                        .filter(|value| (1..=10_000).contains(value))
                })
                .collect::<Option<Vec<_>>>()
                .ok_or("invalid cell size")?;
            context.cell_width = values[0];
            context.cell_height = values[1];
            context.size_requested = true;
            context.resize_order = self.engine.sequence + 1;
            self.sizes_pending = true;
            return Ok(Step::Done(Outcome {
                changed: true,
                ..Outcome::default()
            }));
        }
        if name == b"tmuxx-input" {
            let input = self.request_stdin.take();
            let result = self.native_input(id, command, input).map(Step::Done);
            if result.is_ok() {
                context.activity = self.engine.now;
            }
            return result;
        }
        if name == b"tmuxx-host-state" {
            let input = self.request_stdin.take();
            return self.view_state(id, command, input, context).map(Step::Done);
        }
        if name == b"tmuxx-host-reply" {
            let input = self.request_stdin.take();
            return self.respond_host(id, &command.args, input).map(Step::Done);
        }
        if matches!(
            name,
            b"tmuxx-retain-pane" | b"tmuxx-claim-pane" | b"tmuxx-release-pane"
        ) {
            return self.pane_retention(id, command).map(Step::Done);
        }
        if name == b"tmuxx-source-diagnostics" {
            return Ok(Step::SourceResult(
                command.args[1].clone(),
                !context.control,
            ));
        }
        if matches!(name, b"source-file" | b"source") {
            let args = Args::parse(&command.args[1..], "Fnqt:v")?;
            if depth >= 50 {
                return Err("too many nested files".into());
            }
            let target = self.hook_target(args.get(b't'), context)?;
            let mut source = Box::new(SourceFiles {
                paths: VecDeque::new(),
                context: target,
                depth,
                parse_only: args.has(b'n'),
                verbose: args.has(b'v') && !context.control,
                groups: Vec::new(),
                diagnostics: Vec::new(),
                trace: Vec::new(),
                error: None,
                control_output: Vec::new(),
                deferred_output: Vec::new(),
            });
            // C expands and validates all paths before preparing any file.
            for raw in &args.rest {
                let raw = if args.has(b'F') {
                    self.expand_job(raw, args.get(b't'), &source.context, &[])?
                } else {
                    raw.clone()
                };
                if raw == b"-" {
                    source.paths.push_back(PathBuf::from("-"));
                    continue;
                }
                let mut path = PathBuf::from(os_string(&raw).map_err(|error| error.to_string())?);
                if path.is_relative()
                    && let Some(cwd) = &context.cwd
                {
                    let cwd = PathBuf::from(cwd);
                    path = cwd.canonicalize().unwrap_or(cwd).join(path);
                }
                match source_paths(&path, args.has(b'q')) {
                    Ok(paths) => source.paths.extend(paths),
                    Err(error) => {
                        let error = error.strip_suffix(&*path.to_string_lossy()).map_or_else(
                            || error.clone(),
                            |prefix| format!("{prefix}{}", String::from_utf8_lossy(&raw)),
                        );
                        source.error(error);
                        source.control_output.append(&mut source.deferred_output);
                    }
                }
                if source.paths.len() > 4096 {
                    return Err("source-file glob exceeds path limit".into());
                }
            }
            if source.paths.is_empty()
                && let Some(error) = source.error
            {
                return Err(error);
            }
            return self.continue_source(id, source);
        }
        if matches!(name, b"wait-for" | b"wait") {
            return self.wait_command(id, &command.args[1..], group);
        }
        if matches!(name, b"run-shell" | b"run" | b"if-shell" | b"if") {
            return self.job_command(id, command, context, depth, group);
        }
        if matches!(
            name,
            b"set-option" | b"set" | b"set-window-option" | b"setw"
        ) {
            let args = Args::parse(&command.args[1..], "aFgopqst:uUw")?;
            if !args.has(b'u')
                && args
                    .rest
                    .first()
                    .is_some_and(|key| key.len() >= 9 && b"default-shell".starts_with(key))
                && let Some(raw) = args.rest.get(1)
            {
                let mut value = if args.has(b'F') {
                    self.expand_job(raw, args.get(b't'), context, &[])?
                } else {
                    raw.clone()
                };
                if args.has(b'a') {
                    let mut previous =
                        self.expand_job(b"#{default-shell}", args.get(b't'), context, &[])?;
                    previous.extend(value);
                    value = previous;
                }
                if !suitable_shell(&value) {
                    return Err(format!(
                        "not a suitable shell: {}",
                        String::from_utf8_lossy(&value)
                    ));
                }
            }
        }
        if matches!(name, b"load-buffer" | b"loadb" | b"save-buffer" | b"saveb") {
            let mut out = self.engine.execute(command, context)?;
            if matches!(name, b"save-buffer" | b"saveb")
                && out.effects.is_empty()
                && self
                    .control
                    .clients
                    .get(&id)
                    .is_some_and(|client| client.wire)
            {
                out.effects.push(Effect::SaveBuffer {
                    path: b"-".to_vec(),
                    data: std::mem::take(&mut out.output),
                    append: false,
                });
            }
            if let Some(mut effect) = out.effects.pop() {
                let (Effect::LoadBuffer { path, .. } | Effect::SaveBuffer { path, .. }) =
                    &mut effect
                else {
                    unreachable!()
                };
                if path != b"-"
                    && std::path::Path::new(&os_string(path).map_err(|error| error.to_string())?)
                        .is_relative()
                    && let Some(cwd) = &context.cwd
                {
                    *path = path_bytes(
                        &PathBuf::from(cwd)
                            .join(os_string(path).map_err(|error| error.to_string())?),
                    )?;
                }
                if self.file_clients.contains(&id)
                    || (self.scheduler.stdin_clients.contains(&id)
                        && !context.control
                        && !context.attached
                        && self.request_stdin.is_none()
                        && matches!(&effect, Effect::LoadBuffer { path, .. } if path == b"-"))
                {
                    return Ok(Step::File(PendingFile::Buffer(effect)));
                }
                if self
                    .control
                    .clients
                    .get(&id)
                    .is_some_and(|client| client.wire)
                {
                    return Ok(Step::NativeFile(effect));
                }
                self.effects(vec![effect])?;
                out.notifications.append(&mut self.effect_notifications);
            }
            return Ok(Step::Done(out));
        }
        let out = self.execute(command, context)?;
        Ok(Step::Done(out))
    }

    fn collect_source(
        &mut self,
        source: &mut SourceFiles,
        input: &[u8],
        path: &str,
    ) -> Result<(), String> {
        match self.prepare_source(input, &source.context, source.parse_only) {
            Ok(prepared) => {
                if source.verbose {
                    for (line, commands) in &prepared.groups {
                        source.trace.extend(format!("{path}:{line}: ").as_bytes());
                        source
                            .trace
                            .extend(super::hooks::printed_commands(commands));
                        source.trace.push(b'\n');
                    }
                }
                if let Some(error) = prepared.error {
                    source
                        .diagnostics
                        .extend(format!("{path}:{error}\n").as_bytes());
                } else if !source.parse_only {
                    source
                        .groups
                        .extend(prepared.groups.into_iter().map(|(_, commands)| commands));
                }
            }
            Err(error) => source
                .diagnostics
                .extend(format!("{path}:{error}\n").as_bytes()),
        }
        if source.groups.iter().map(Vec::len).sum::<usize>() > MAX_COMMANDS
            || command_size(source.groups.iter().flatten()) > MAX_REQUEST_BYTES
            || source.trace.len() + source.diagnostics.len() > MAX_REQUEST_BYTES
        {
            return Err("configuration command queue exceeds limit".into());
        }
        Ok(())
    }

    fn continue_source(&mut self, id: u64, mut source: Box<SourceFiles>) -> Result<Step, String> {
        while let Some(path) = source.paths.pop_front() {
            if path == Path::new("-") {
                if source.context.control
                    || source.context.attached
                    || self.scheduler.internal.contains_key(&id)
                {
                    source.error("Bad file descriptor: -".into());
                } else if let Some(input) = self.request_stdin.take() {
                    self.collect_source(&mut source, &input, "-")?;
                } else if self.file_clients.contains(&id)
                    || self.scheduler.stdin_clients.contains(&id)
                {
                    return Ok(Step::File(PendingFile::Source(source)));
                } else {
                    source.error("Bad file descriptor: -".into());
                }
                continue;
            }
            let input = (|| {
                let mut input = Vec::new();
                super::files::open(std::fs::OpenOptions::new().read(true), &path)?
                    .take((MAX_REQUEST_BYTES + 1) as u64)
                    .read_to_end(&mut input)?;
                Ok::<_, io::Error>(input)
            })();
            match input {
                Ok(input) if input.len() <= MAX_REQUEST_BYTES => {
                    self.collect_source(&mut source, &input, &path.to_string_lossy())?
                }
                Ok(_) => return Err("configuration exceeds size limit".into()),
                Err(error) => {
                    let error = os_error_message(&error);
                    source.error(format!("{error}: {}", path.display()));
                }
            }
        }
        if !source.diagnostics.is_empty() {
            source.groups.push(vec![Command::new(vec![
                b"tmuxx-source-diagnostics".to_vec(),
                std::mem::take(&mut source.diagnostics),
            ])]);
        }
        Ok(Step::InsertSource(source))
    }

    pub(super) fn prepare_commands(
        &mut self,
        input: &[u8],
        context: &ClientContext,
    ) -> Result<Vec<Command>, String> {
        let source = self.prepare_source(input, context, false)?;
        if let Some(error) = source.error {
            return Err(error);
        }
        Ok(source
            .groups
            .into_iter()
            .flat_map(|(_, commands)| commands)
            .collect())
    }

    fn argument_commands(
        &mut self,
        argument: &CommandsArgument,
        context: &ClientContext,
    ) -> Result<Vec<Command>, String> {
        match argument {
            CommandsArgument::Source(source) => self.prepare_commands(source, context),
            CommandsArgument::Parsed(commands) => Ok(commands.clone()),
        }
    }

    fn prepare_source(
        &mut self,
        input: &[u8],
        context: &ClientContext,
        parse_only: bool,
    ) -> Result<PreparedSource, String> {
        let mut context = context.clone();
        context.no_format_jobs = true;
        let mut environment: tmuxx_config::Environment = self
            .engine
            .environment
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect();
        environment.hidden = self
            .engine
            .hidden_environment
            .iter()
            .map(|key| key.as_bytes().to_vec())
            .collect();
        let commands = tmuxx_config::prepare_located_with_home(
            input,
            &mut environment,
            |key: &[u8]| {
                let mut format = b"#{".to_vec();
                format.extend_from_slice(key);
                format.push(b'}');
                self.engine
                    .expand(&format, context.session, None, None, &context)
                    .ok()
            },
            parse_only,
            super::defaults::user_home,
        )
        .map_err(|error| format!("{}: {}", error.line, error.message))?;
        // Convert the prepared environment in full before committing any of it.
        // A malformed value must not change a literal-U+FFFD variable or leave
        // earlier assignments from the same failed preparation partially live.
        let hidden_environment = environment
            .hidden
            .into_iter()
            .map(|key| {
                String::from_utf8(key).map_err(|_| "non-UTF8 environment names are unsupported")
            })
            .collect::<Result<_, _>>()?;
        let values = environment
            .values
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    String::from_utf8(key)
                        .map_err(|_| "non-UTF8 environment names are unsupported")?,
                    String::from_utf8(value)
                        .map_err(|_| "non-UTF8 environment values are unsupported")?,
                ))
            })
            .collect::<Result<_, &str>>()?;
        self.engine.hidden_environment = hidden_environment;
        self.engine.environment = values;
        self.engine
            .removed_environment
            .retain(|key| !self.engine.environment.contains_key(key));
        let mut groups: Vec<(usize, Vec<Command>)> = Vec::new();
        for (command, offset) in commands {
            let line = tmuxx_command::Error::at(input, offset, "").line;
            let commands = match self.prepare_aliases(vec![command]) {
                Ok(commands) => commands,
                Err(error) => {
                    if groups.last().is_some_and(|(previous, _)| *previous == line) {
                        groups.pop();
                    }
                    return Ok(PreparedSource {
                        groups,
                        error: Some(format!("{line}: {error}")),
                    });
                }
            };
            if let Some((previous, group)) = groups.last_mut()
                && *previous == line
            {
                group.extend(commands);
            } else {
                groups.push((line, commands));
            }
        }
        Ok(PreparedSource {
            groups,
            error: None,
        })
    }

    fn prepare_aliases(&self, commands: Vec<Command>) -> Result<Vec<Command>, String> {
        self.prepare_aliases_at_depth(commands, 0)
    }
    fn prepare_aliases_at_depth(
        &self,
        commands: Vec<Command>,
        depth: usize,
    ) -> Result<Vec<Command>, String> {
        if depth > 100 {
            return Err("command nesting limit exceeded".into());
        }
        let aliases = self.engine.option_array(None, "command-alias");
        let mut result = Vec::new();
        for command in commands {
            let alias = aliases
                .iter()
                .filter_map(|(_, entry)| entry.split_once('='))
                .find(|(name, _)| {
                    command
                        .args
                        .first()
                        .is_some_and(|arg| arg == name.as_bytes())
                });
            if let Some((_, value)) = alias {
                let mut expanded = tmuxx_command::parse_with_home(
                    value.as_bytes(),
                    |key| {
                        std::str::from_utf8(key)
                            .ok()
                            .and_then(|key| self.engine.environment.get(key))
                            .map(|value| value.as_bytes().to_vec())
                    },
                    super::defaults::user_home,
                )
                .map_err(|error| error.to_string())?;
                if let Some(last) = expanded.last_mut() {
                    let start = last.args.len() - 1;
                    last.blocks.extend(
                        command
                            .blocks
                            .into_iter()
                            .map(|(index, block)| (start + index, block)),
                    );
                    last.args.extend(command.args.into_iter().skip(1));
                }
                result.extend(expanded);
            } else {
                result.push(command);
            }
            if result.len() > MAX_COMMANDS {
                return Err("command alias expansion exceeds limit".into());
            }
        }
        for command in &mut result {
            for (index, block) in &mut command.blocks {
                *block = self.prepare_aliases_at_depth(std::mem::take(block), depth + 1)?;
                command.args[*index] = crate::hooks::printed_commands(block);
            }
            if command.args.first().is_some_and(|name| {
                matches!(
                    name.as_slice(),
                    b"tmuxx-input"
                        | b"tmuxx-cell-size"
                        | b"tmuxx-host-state"
                        | b"tmuxx-host-reply"
                        | b"tmuxx-retain-pane"
                        | b"tmuxx-claim-pane"
                        | b"tmuxx-release-pane"
                )
            }) {
                continue;
            }
            let entry = tmuxx_command::registry::validate(command)?;
            command.args[0] = entry.name.as_bytes().to_vec();
        }
        Ok(result)
    }

    fn wait_command(&mut self, id: u64, argv: &[Vec<u8>], group: u64) -> Result<Step, String> {
        let args = Args::parse(argv, "EF:LSUlvw:")?;
        if args.rest.len() != 1 {
            return Err("wait-for requires one channel".into());
        }
        let name = &args.rest[0];
        if self.scheduler.startup == Some(id) && !args.has(b'S') && !args.has(b'U') {
            return Err(if args.has(b'L') {
                "not able to lock"
            } else {
                "not able to wait"
            }
            .into());
        }
        if name.len() > 1024 {
            return Err("wait channel name exceeds limit".into());
        }
        if args.has(b'E') {
            if !super::hooks::valid_event(name) {
                return Err(format!("invalid event: {}", String::from_utf8_lossy(name)));
            }
            if args.has(b'l') {
                let output = self
                    .scheduler
                    .event_waiters
                    .iter()
                    .filter(|(_, waiter)| waiter.name == *name)
                    .flat_map(|(id, _)| {
                        format!(
                            "{}\n",
                            self.clients
                                .get(id)
                                .map(|c| c.context.client_name.as_str())
                                .unwrap_or_default()
                        )
                        .into_bytes()
                    })
                    .collect();
                return Ok(Step::Done(Outcome {
                    output,
                    ..Outcome::default()
                }));
            }
            if let Some(who) = args.get(b'w') {
                let waiter =
                    self.scheduler
                        .event_waiters
                        .iter()
                        .find(|(id, waiter)| {
                            waiter.name == *name
                                && self.clients.get(id).is_some_and(|client| {
                                    who == client.context.client_name.as_bytes()
                                })
                        })
                        .map(|(id, _)| *id);
                if let Some(id) = waiter {
                    self.scheduler.event_waiters.remove(&id);
                    self.scheduler.unblock(id);
                    return Ok(Step::Done(Outcome::default()));
                }
                return Err(format!("waiter {} not found", String::from_utf8_lossy(who)));
            }
            if self.scheduler.event_waiters.len() >= MAX_CHANNELS {
                return Err("event waiter limit exceeded".into());
            }
            self.scheduler.event_waiters.insert(
                id,
                EventWaiter {
                    name: name.clone(),
                    filter: args.get(b'F').map(<[u8]>::to_vec),
                    verbose: args.has(b'v'),
                    group,
                },
            );
            return Ok(Step::Wait);
        }
        if args.has(b'l') {
            let output = self
                .scheduler
                .channels
                .get(name)
                .map(|channel| {
                    channel
                        .waiters
                        .iter()
                        .chain(&channel.lockers)
                        .flat_map(|id| {
                            format!(
                                "{}\n",
                                self.clients
                                    .get(id)
                                    .map(|c| c.context.client_name.as_str())
                                    .unwrap_or_default()
                            )
                            .into_bytes()
                        })
                        .collect()
                })
                .unwrap_or_default();
            return Ok(Step::Done(Outcome {
                output,
                ..Outcome::default()
            }));
        }
        if let Some(who) = args.get(b'w') {
            let mut wake = None;
            if let Some(channel) = self.scheduler.channels.get_mut(name) {
                for list in [&mut channel.waiters, &mut channel.lockers] {
                    if let Some(index) = list.iter().position(|id| {
                        self.clients
                            .get(id)
                            .is_some_and(|client| who == client.context.client_name.as_bytes())
                    }) {
                        wake = list.remove(index);
                        break;
                    }
                }
            }
            self.scheduler.clean_channel(name);
            if let Some(id) = wake {
                self.scheduler.unblock(id);
            }
            return Ok(Step::Done(Outcome::default()));
        }
        if args.has(b'U')
            && !args.has(b'S')
            && !args.has(b'L')
            && !self
                .scheduler
                .channels
                .get(name)
                .is_some_and(|channel| channel.locked)
        {
            return Err(format!(
                "channel {} not locked",
                String::from_utf8_lossy(name)
            ));
        }
        if !self.scheduler.channels.contains_key(name)
            && self.scheduler.channels.len() >= MAX_CHANNELS
        {
            return Err("wait channel limit exceeded".into());
        }
        let channel = self.scheduler.channels.entry(name.clone()).or_default();
        let mut wake = Vec::new();
        let blocked;
        let mut consume_woken = false;
        if args.has(b'S') {
            if channel.waiters.is_empty() && !channel.woken {
                channel.woken = true;
            } else {
                wake.extend(channel.waiters.drain(..));
                consume_woken = true;
            }
            blocked = false;
        } else if args.has(b'L') {
            blocked = channel.locked;
            if blocked {
                channel.lockers.push_back(id);
            } else {
                channel.locked = true;
            }
        } else if args.has(b'U') {
            if !channel.locked {
                return Err(format!(
                    "channel {} not locked",
                    String::from_utf8_lossy(name)
                ));
            }
            if let Some(id) = channel.lockers.pop_front() {
                wake.push(id);
            } else {
                channel.locked = false;
                consume_woken = true;
            }
            blocked = false;
        } else if channel.woken {
            consume_woken = true;
            blocked = false;
        } else {
            channel.waiters.push_back(id);
            blocked = true;
        }
        if consume_woken && !channel.locked && channel.waiters.is_empty() {
            channel.woken = false;
        }
        self.scheduler.clean_channel(name);
        for id in wake {
            self.scheduler.unblock(id);
        }
        Ok(if blocked {
            Step::Wait
        } else {
            Step::Done(Outcome::default())
        })
    }

    pub(super) fn signal_hook_event(&mut self, name: &str, context: &ClientContext) {
        let waiters = std::mem::take(&mut self.scheduler.event_waiters);
        for (id, waiter) in waiters {
            if waiter.name != name.as_bytes() {
                self.scheduler.event_waiters.insert(id, waiter);
                continue;
            }
            let mut event_context = context.clone();
            event_context.formats = context
                .formats
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix(b"hook_")
                        .map(|key| (key.to_vec(), value.clone()))
                })
                .collect();
            let mut result = waiter
                .filter
                .as_deref()
                .map(|filter| {
                    let mut filter_context = ClientContext {
                        formats: event_context.formats.clone(),
                        ..ClientContext::default()
                    };
                    // C built-in callbacks shadow payload names, with no target defaults.
                    for key in [
                        b"session_name".as_slice(),
                        b"window_name",
                        b"window_index",
                        b"pane_current_path",
                    ] {
                        filter_context.formats.remove(key);
                    }
                    tmuxx_format::expand_no_jobs(filter, |key| {
                        let mut field = b"#{".to_vec();
                        field.extend_from_slice(key);
                        field.push(b'}');
                        self.engine
                            .expand(&field, None, None, None, &filter_context)
                            .ok()
                    })
                    .map(|value| tmuxx_format::truthy(&value))
                    .map_err(|error| error.to_string())
                })
                .unwrap_or(Ok(true));
            if waiter.verbose
                && let Some(batch) = self
                    .scheduler
                    .queues
                    .get_mut(&id)
                    .and_then(|queue| queue.pending.front_mut())
            {
                for (key, value) in &event_context.formats {
                    if key
                        .strip_suffix(b"_name")
                        .is_some_and(|base| event_context.formats.contains_key(base))
                    {
                        continue;
                    }
                    batch.output.extend(key);
                    batch.output.push(b'=');
                    batch.output.extend(value);
                    batch.output.push(b'\n');
                }
                if batch.output.len() > MAX_REQUEST_BYTES {
                    batch.output.clear();
                    result = Err("event output exceeds limit".into());
                }
            }
            match result {
                Ok(false) => {
                    self.scheduler.event_waiters.insert(id, waiter);
                }
                Ok(true) => self.scheduler.unblock(id),
                Err(error) => {
                    if let Some(batch) = self
                        .scheduler
                        .queues
                        .get_mut(&id)
                        .and_then(|queue| queue.pending.front_mut())
                    {
                        batch.fail(waiter.group, error);
                    }
                    self.scheduler.unblock(id);
                }
            }
        }
    }

    fn job_command(
        &mut self,
        id: u64,
        invocation: &Command,
        context: &ClientContext,
        depth: usize,
        group: u64,
    ) -> Result<Step, String> {
        let name = invocation.args[0].as_slice();
        let conditional = matches!(name, b"if-shell" | b"if");
        let args = Args::parse(
            &invocation.args[1..],
            if conditional { "bFt:" } else { "bCd:Ec:s:t:" },
        )?;
        if conditional && !(2..=3).contains(&args.rest.len()) {
            return Err("if-shell requires a condition and command [else-command]".into());
        }
        let delay = args
            .get(b'd')
            .map(job_delay)
            .transpose()?
            .unwrap_or_default();
        if !conditional && args.rest.is_empty() && !args.has(b'd') {
            return Ok(Step::Done(Outcome::default()));
        }
        // Targeted run-shell output requires a native view-mode adapter.
        if !conditional && !args.has(b'C') && args.has(b't') {
            return Err("run-shell -t output presentation is unsupported".into());
        }
        let target = args.get(b't');
        let resolved_context = self.hook_target(target, context)?;
        let context = &resolved_context;
        let mut command = args.rest.first().cloned().unwrap_or_default();
        if !args.has(b'C') || invocation.argument_block(&args, 0).is_none() {
            command = self.expand_job(
                &command,
                target,
                context,
                if args.has(b'C') { &[] } else { &args.rest },
            )?;
        }
        if conditional && args.has(b'F') {
            let index = if command.first().is_some_and(|b| *b != b'0') {
                1
            } else {
                2
            };
            let branch = CommandsArgument::get(invocation, &args, index);
            return match branch {
                Some(branch) => self
                    .argument_commands(&branch, context)
                    .map(|commands| Step::InsertContext(commands, context.clone())),
                None => Ok(Step::Done(Outcome::default())),
            };
        }
        if self.scheduler.jobs.len() >= MAX_JOBS {
            return Err("job limit exceeded".into());
        }
        let cwd = if let Some(path) = args.get(b'c') {
            PathBuf::from(
                os_string(&self.expand_job(path, target, context, &args.rest)?)
                    .map_err(|error| error.to_string())?,
            )
        } else if let Some(path) = &context.cwd {
            PathBuf::from(path)
        } else {
            std::env::current_dir().map_err(|error| error.to_string())?
        };
        let mut environment = if let Some(sid) = context
            .session
            .filter(|sid| self.engine.sessions.contains_key(sid))
        {
            self.engine.spawn_environment(sid)
        } else {
            self.engine
                .environment
                .iter()
                .filter(|(key, _)| {
                    !self.engine.hidden_environment.contains(*key)
                        && !self.engine.removed_environment.contains(*key)
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        };
        if self.scheduler.startup.is_none() {
            environment.insert(
                "TERM".into(),
                self.engine
                    .server_options
                    .get("default-terminal")
                    .cloned()
                    .unwrap_or_else(|| "tmux-256color".into()),
            );
            environment.insert("TERM_PROGRAM".into(), "tmuxx".into());
            environment.insert(
                "TERM_PROGRAM_VERSION".into(),
                env!("CARGO_PKG_VERSION").into(),
            );
            environment.insert("COLORTERM".into(), "truecolor".into());
        }
        if let Some(endpoint) = &self.tmux_endpoint {
            let sid = context
                .session
                .map_or_else(|| "-1".into(), |sid| sid.to_string());
            environment.insert(
                "TMUX".into(),
                format!("{},{},{sid}", endpoint.display(), std::process::id()),
            );
        }
        let job_id = self.scheduler.next_job;
        self.scheduler.next_job += 1;
        let stop = Arc::new(AtomicBool::new(false));
        let background = args.has(b'b');
        let commands_only = args.has(b'C');
        let thread = spawn_job(
            job_id,
            command.clone(),
            super::defaults::system_shell(),
            cwd,
            environment,
            delay,
            args.has(b'E'),
            commands_only,
            false,
            stop.clone(),
            self.tx.clone(),
        )?;
        let job = Job {
            client: id,
            context: context.clone(),
            background,
            then: if conditional {
                CommandsArgument::get(invocation, &args, 1)
            } else if commands_only {
                Some(invocation.argument_block(&args, 0).map_or_else(
                    || CommandsArgument::Source(command.clone()),
                    |block| CommandsArgument::Parsed(block.to_vec()),
                ))
            } else {
                None
            },
            otherwise: conditional
                .then(|| CommandsArgument::get(invocation, &args, 2))
                .flatten(),
            command,
            conditional,
            commands_only,
            depth,
            group,
            stop,
            thread: Some(thread),
        };
        self.scheduler.jobs.insert(job_id, job);
        Ok(if background {
            Step::Done(Outcome::default())
        } else {
            Step::Wait
        })
    }

    pub(super) fn expand_job(
        &mut self,
        input: &[u8],
        target: Option<&[u8]>,
        context: &ClientContext,
        positional: &[Vec<u8>],
    ) -> Result<Vec<u8>, String> {
        let mut context = context.clone();
        if let Some(command) = context.command {
            context
                .formats
                .entry(b"command".to_vec())
                .or_insert_with(|| command.as_bytes().to_vec());
        }
        let mut args = vec![b"display-message".to_vec(), b"-p".to_vec()];
        if let Some(target) = target {
            args.extend([b"-t".to_vec(), target.to_vec()]);
        }
        // Extra run-shell arguments are format variables #{1}, #{2}, ... .
        let mut failure = None;
        let input = tmuxx_format::expand(input, |key| {
            if let Some(index) = std::str::from_utf8(key)
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|n| *n != 0)
            {
                return positional.get(index).cloned();
            }
            let mut format = b"#{".to_vec();
            format.extend_from_slice(key);
            format.push(b'}');
            let mut query = args.clone();
            query.push(format);
            match self.engine.execute(
                &Command {
                    blocks: Default::default(),
                    args: query,
                },
                &mut context.clone(),
            ) {
                Ok(mut out) => {
                    out.output.pop(); // display-message -p adds exactly one newline.
                    Some(out.output)
                }
                Err(error) => {
                    failure = Some(error);
                    None
                }
            }
        })
        .map_err(|error| error.to_string())?;
        if let Some(error) = failure {
            return Err(error);
        }
        // Validate an explicit target even if the format did not reference fields.
        if target.is_some() {
            let mut query = args;
            query.push(b"".to_vec());
            self.engine.execute(
                &Command {
                    blocks: Default::default(),
                    args: query,
                },
                &mut context.clone(),
            )?;
        }
        Ok(input)
    }

    pub(super) fn job_finished(&mut self, result: JobResult) {
        if self.format_job_finished(result.id, &result.output, result.error.is_some()) {
            return;
        }
        let Some(job) = self.scheduler.jobs.remove(&result.id) else {
            return;
        };
        let selected = if job.commands_only {
            job.then.as_ref()
        } else if job.conditional {
            if result.status == 0 {
                job.then.as_ref()
            } else {
                job.otherwise.as_ref()
            }
        } else {
            None
        };
        let commands = selected
            .map(|argument| self.argument_commands(argument, &job.context))
            .transpose();
        let mut output = Vec::new();
        if !job.conditional && !job.commands_only {
            // C prints each job-output line as a C string, including a final LF.
            for raw in result.output.split_inclusive(|byte| *byte == b'\n') {
                let line = raw.strip_suffix(b"\n").unwrap_or(raw);
                output.extend_from_slice(
                    &line[..line
                        .iter()
                        .position(|byte| *byte == 0)
                        .unwrap_or(line.len())],
                );
                output.push(b'\n');
            }
            if result.status != 0 {
                let suffix = if let Some(signal) = result.signal {
                    format!("terminated by signal {signal}")
                } else {
                    format!("returned {}", result.status)
                };
                output.extend_from_slice(
                    format!("'{}' {suffix}\n", String::from_utf8_lossy(&job.command)).as_bytes(),
                );
            }
        }
        if job.background {
            let error = match commands {
                Ok(Some(commands)) => {
                    if self
                        .control
                        .clients
                        .get(&job.client)
                        .is_some_and(|client| client.wire)
                        && self.clients.contains_key(&job.client)
                    {
                        self.background_control_commands(
                            job.client,
                            commands,
                            job.context.clone(),
                            job.depth + 1,
                        );
                    } else {
                        self.internal_queue(job.context.clone(), commands, job.depth + 1, false);
                    }
                    result.error
                }
                Err(error) => Some(error),
                _ => result.error,
            };
            if self.clients.contains_key(&job.client) && (!output.is_empty() || error.is_some()) {
                let text = error.map(String::into_bytes).unwrap_or(output);
                let data = [
                    b"%message ".as_slice(),
                    tmuxx_protocol::escape_output(&text).as_slice(),
                ]
                .concat();
                self.send(job.client, ServerMessage::Control { data });
            }
        } else {
            let deferred_error = result
                .error
                .clone()
                .or_else(|| commands.as_ref().err().cloned());
            let mut late_output = None;
            if let Some(batch) = self
                .scheduler
                .queues
                .get_mut(&job.client)
                .and_then(|queue| queue.pending.front_mut())
            {
                batch.output.extend(output);
                if !job.conditional && !job.commands_only {
                    batch.status = Some(result.status);
                }
                if let Some(error) = result.error {
                    batch.fail(job.group, error);
                }
                match commands {
                    Ok(Some(commands)) => batch.insert(
                        commands,
                        job.depth + 1,
                        job.group,
                        Some(job.context.clone()),
                    ),
                    Err(error) => batch.fail(job.group, error),
                    _ => {}
                }
                if batch.output.len() > MAX_REQUEST_BYTES {
                    batch.output.clear();
                    batch.fail(job.group, "command output exceeds limit".into());
                }
                if batch.control_guards.is_some() {
                    let mut data = std::mem::take(&mut batch.output);
                    if let Some(error) = deferred_error {
                        data.extend(format!("{error}\n").into_bytes());
                    }
                    late_output = Some(data);
                }
            }
            if let Some(data) = late_output {
                self.control_unframed_output(job.client, data);
            }
            self.scheduler.unblock(job.client);
        }
    }
}

fn native_control_file(effect: Effect) -> Result<Vec<u8>, String> {
    let (path, result) = match effect {
        Effect::LoadBuffer { path, .. } => {
            let result = if path == b"-" {
                // This is the protocol's unavailable stdin resource, not a
                // Win32 last-error code (numeric errno 9 has another meaning).
                return Err("Bad file descriptor: -".into());
            } else {
                (|| {
                    let mut data = Vec::new();
                    files::open(
                        std::fs::OpenOptions::new().read(true),
                        Path::new(&os_string(&path)?),
                    )?
                    .take((MAX_REQUEST_BYTES + 1) as u64)
                    .read_to_end(&mut data)?;
                    Ok(data)
                })()
            };
            (path, result)
        }
        Effect::SaveBuffer { path, data, append } => {
            let result = if path == b"-" {
                return Err("Bad file descriptor: -".into());
            } else {
                files::open(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(!append)
                        .append(append),
                    Path::new(&os_string(&path).map_err(|error| error.to_string())?),
                )
                .and_then(|mut file| file.write_all(&data))
                .map(|()| Vec::new())
            };
            (path, result)
        }
        _ => unreachable!(),
    };
    let data = result.map_err(|error| files::path_error(&error, &path))?;
    if data.len() > MAX_REQUEST_BYTES {
        return Err(format!(
            "file input exceeds limit: {}",
            files::diagnostic_path(&path)
        ));
    }
    Ok(data)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_job(
    id: u64,
    command: Vec<u8>,
    shell: String,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    delay: Duration,
    stderr: bool,
    commands_only: bool,
    format_output: bool,
    stop: Arc<AtomicBool>,
    tx: SyncSender<Event>,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name(format!("tmuxx-job-{id}"))
        .spawn(move || {
            #[cfg(unix)]
            use std::os::fd::OwnedFd;
            #[cfg(unix)]
            use std::os::unix::process::{CommandExt, ExitStatusExt};
            let mut result = JobResult {
                id,
                output: Vec::new(),
                status: 0,
                signal: None,
                error: None,
            };
            let deadline = Instant::now() + delay;
            while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
                thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            if !commands_only && !command.is_empty() {
                let execution = (|| -> io::Result<()> {
                    #[cfg(unix)]
                    let (mut reader, mut child) = {
                        let (mut reader, writer) = std::os::unix::net::UnixStream::pair()?;
                        reader.set_nonblocking(true)?;
                        let mut process = ProcessCommand::new(&shell);
                        process
                            .arg0(
                                Path::new(&shell)
                                    .file_name()
                                    .unwrap_or_else(|| shell.as_ref()),
                            )
                            .arg("-c")
                            .arg(os_string(&command)?)
                            .current_dir(&cwd)
                            .env_clear()
                            .envs(&environment)
                            .env("PWD", &cwd)
                            .stdin(Stdio::from(OwnedFd::from(writer.try_clone()?)))
                            .stderr(if stderr {
                                Stdio::from(OwnedFd::from(writer.try_clone()?))
                            } else {
                                Stdio::null()
                            })
                            .stdout(Stdio::from(OwnedFd::from(writer)))
                            .process_group(0);
                        let mut child = process.spawn().or_else(|_| {
                            let home = std::env::var_os("HOME")
                                .filter(|home| !home.is_empty())
                                .or_else(|| {
                                    super::defaults::user_home(b"")
                                        .and_then(|home| os_string(&home).ok())
                                });
                            if let Some(home) = home {
                                process.current_dir(&home).env("PWD", &home);
                                if let Ok(child) = process.spawn() {
                                    return Ok(child);
                                }
                            }
                            process.current_dir("/").env("PWD", "/").spawn()
                        })?;
                        drop(process);
                        reader.shutdown(std::net::Shutdown::Write)?;
                        (reader, child)
                    };
                    #[cfg(windows)]
                    let (mut reader, mut child) = {
                        let mut directories = vec![cwd];
                        let home = std::env::var_os("HOME")
                            .filter(|home| !home.is_empty())
                            .or_else(|| {
                                super::defaults::user_home(b"")
                                    .and_then(|home| os_string(&home).ok())
                            });
                        if let Some(home) = home {
                            directories.push(PathBuf::from(home));
                        }
                        // The native counterpart of / is the system volume's
                        // root, not the owner's arbitrary working directory.
                        let system_shell = PathBuf::from(super::defaults::system_shell());
                        directories.push(system_shell.ancestors().last().unwrap().to_path_buf());
                        let mut child = spawn_windows_job_in_directories(
                            &shell,
                            &command,
                            &environment,
                            stderr,
                            &directories,
                        )?;
                        let reader = child
                            .stdout
                            .take()
                            .ok_or_else(|| io::Error::other("job output pipe missing"))?;
                        (reader, child)
                    };
                    #[cfg(unix)]
                    let child_id = child.id();
                    let completion = (|| -> io::Result<std::process::ExitStatus> {
                        let mut eof = false;
                        let mut status = None;
                        let mut buffer = [0; 65536];
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return Err(io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "job cancelled",
                                ));
                            }
                            if !eof {
                                match reader.read(&mut buffer) {
                                    Ok(0) => eof = true,
                                    Ok(n) => {
                                        if result.output.len().saturating_add(n) > MAX_REQUEST_BYTES
                                        {
                                            return Err(io::Error::other(
                                                "job output exceeds limit",
                                            ));
                                        }
                                        result.output.extend_from_slice(&buffer[..n]);
                                        if format_output
                                            && let Some(end) = result
                                                .output
                                                .iter()
                                                .rposition(|byte| *byte == b'\n')
                                        {
                                            let begin = result.output[..end]
                                                .iter()
                                                .rposition(|byte| *byte == b'\n')
                                                .map_or(0, |index| index + 1);
                                            let line = result.output[begin..end]
                                                .strip_suffix(b"\r")
                                                .unwrap_or(&result.output[begin..end])
                                                .to_vec();
                                            result.output.drain(..=end);
                                            let mut event = Event::FormatJobLine(id, line);
                                            while !stop.load(Ordering::Acquire) {
                                                match tx.try_send(event) {
                                                    Ok(()) | Err(TrySendError::Disconnected(_)) => {
                                                        break;
                                                    }
                                                    Err(TrySendError::Full(value)) => {
                                                        event = value;
                                                        thread::sleep(Duration::from_millis(1));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                        continue;
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                            if status.is_none() {
                                status = child.try_wait()?;
                            }
                            if eof && let Some(status) = status {
                                return Ok(status);
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                    })();
                    // Descendants can keep stdout open after the shell exits.
                    if completion.is_err() {
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(-(child_id as i32), libc::SIGKILL);
                        }
                        #[cfg(windows)]
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    let status = completion?;
                    #[cfg(unix)]
                    {
                        result.status = status
                            .code()
                            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
                        result.signal = status.signal();
                    }
                    #[cfg(windows)]
                    {
                        result.status = status.code().unwrap_or(1);
                    }
                    Ok(())
                })();
                if let Err(error) = execution {
                    result.error = Some(error.to_string());
                    result.status = 1;
                }
            }
            let mut event = Event::JobFinished(result);
            while !stop.load(Ordering::Acquire) {
                match tx.try_send(event) {
                    Ok(()) | Err(TrySendError::Disconnected(_)) => break,
                    Err(TrySendError::Full(value)) => {
                        event = value;
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        })
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn spawn_windows_job_in_directories(
    shell: &str,
    command: &[u8],
    environment: &BTreeMap<String, String>,
    stderr: bool,
    directories: &[PathBuf],
) -> io::Result<super::windows_job::Child> {
    let mut environment = environment.clone();
    let mut last_error =
        io::Error::new(io::ErrorKind::InvalidInput, "job needs a working directory");
    for directory in directories {
        // Reject unrepresentable paths instead of silently selecting a fallback
        // that would alias malformed input onto a different working directory.
        let cwd = directory.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "job directory must be Unicode")
        })?;
        if cwd.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in job directory",
            ));
        }
        environment.insert("PWD".into(), cwd.into());
        match super::windows_job::spawn_shell(
            shell,
            command,
            directory,
            &environment,
            false,
            true,
            stderr,
        ) {
            Ok(child) => return Ok(child),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

#[cfg(all(test, windows))]
mod windows_job_directory_tests {
    use super::*;

    #[test]
    fn scheduled_shell_falls_back_to_home_then_volume_root_with_matching_pwd() {
        let root = std::env::temp_dir().join(format!(
            "tmuxx-job-directory-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home 雪 &^");
        std::fs::create_dir_all(&home).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let shell = super::super::defaults::system_shell();
        let volume = Path::new(&shell).ancestors().last().unwrap().to_path_buf();
        let shell = Path::new(&shell)
            .parent()
            .unwrap()
            .join("WindowsPowerShell/v1.0/powershell.exe")
            .into_os_string()
            .into_string()
            .unwrap();
        let missing = root.join("missing");
        let mut environment: BTreeMap<String, String> = std::env::vars().collect();
        environment.insert("PWD".into(), "stale value".into());
        for (directories, expected) in [
            (vec![missing.clone(), home.clone(), volume.clone()], home),
            (
                vec![
                    missing.clone(),
                    missing.join("also-missing"),
                    volume.clone(),
                ],
                volume,
            ),
        ] {
            let mut child = spawn_windows_job_in_directories(
                &shell,
                b"$b=[Text.Encoding]::UTF8.GetBytes([Environment]::CurrentDirectory+[Environment]::NewLine+$env:PWD+[Environment]::NewLine); [Console]::OpenStandardOutput().Write($b,0,$b.Length)",
                &environment,
                true,
                &directories,
            )
            .unwrap();
            let mut reader = child.stdout.take().unwrap();
            let mut output = Vec::new();
            let mut buffer = [0; 4096];
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < deadline, "scheduled job did not finish");
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => output.extend_from_slice(&buffer[..n]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("job output: {error}"),
                }
            }
            assert!(child.wait().unwrap().success());
            let expected = expected.to_str().unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("{expected}\r\n{expected}\r\n")
            );
        }
    }
}

fn job_delay(raw: &[u8]) -> Result<Duration, String> {
    let input = std::ffi::CString::new(raw).map_err(|_| "NUL in delay")?;
    let mut end = std::ptr::null_mut();
    // C uses strtod, including empty input, leading whitespace and hex floats.
    let seconds = unsafe { libc::strtod(input.as_ptr(), &mut end) };
    if end != unsafe { input.as_ptr().add(raw.len()) }.cast_mut() {
        return Err(format!(
            "invalid delay time: {}",
            String::from_utf8_lossy(raw)
        ));
    }
    if !seconds.is_finite() || !(0.0..=86400.0).contains(&seconds) {
        return Err("delay outside 0..86400 seconds".into());
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn path_bytes(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(path.as_os_str().as_bytes().to_vec())
    }
    #[cfg(windows)]
    path.to_str()
        .map(|value| value.as_bytes().to_vec())
        .ok_or_else(|| "Windows paths must be Unicode".into())
}

fn os_error_message(error: &io::Error) -> String {
    files::error_message(error)
}

#[cfg(windows)]
fn source_paths(path: &Path, quiet: bool) -> Result<Vec<PathBuf>, String> {
    use std::path::Component;
    // Expand one directory component at a time. This does not delegate shell
    // parsing, and every returned filename preserves its original Unicode.
    let has_glob = path.components().any(|component| matches!(component, Component::Normal(value) if value.to_str().is_some_and(|value| value.contains(['*', '?', '[']))));
    let mut paths = vec![if has_glob {
        PathBuf::new()
    } else {
        path.to_path_buf()
    }];
    for component in path.components().filter(|_| has_glob) {
        let Component::Normal(component) = component else {
            for path in &mut paths {
                path.push(component.as_os_str());
            }
            continue;
        };
        let pattern = component
            .to_str()
            .ok_or("Windows source paths must be Unicode")?;
        if !pattern.contains(['*', '?', '[']) {
            for path in &mut paths {
                path.push(component);
            }
            continue;
        }
        let mut expanded = Vec::new();
        for directory in paths {
            let entries = match std::fs::read_dir(if directory.as_os_str().is_empty() {
                Path::new(".")
            } else {
                &directory
            }) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or("source glob encountered a non-Unicode filename")?;
                if name.starts_with('.') && !pattern.starts_with('.') {
                    continue;
                }
                if tmuxx_format::glob_match(pattern.as_bytes(), name.as_bytes())
                    .map_err(str::to_owned)?
                {
                    expanded.push(directory.join(entry.file_name()));
                    if expanded.len() > 4096 {
                        return Err("source-file glob exceeds path limit".into());
                    }
                }
            }
        }
        paths = expanded;
    }
    let mut existing = Vec::new();
    for path in paths {
        if files::validate_windows_path(&path).map_err(|error| error.to_string())? {
            existing.push(path);
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(_) => existing.push(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let mut paths = existing;
    paths.sort();
    if paths.is_empty() && !quiet {
        Err(format!("No such file or directory: {}", path.display()))
    } else {
        Ok(paths)
    }
}

#[cfg(unix)]
fn source_paths(path: &Path, quiet: bool) -> Result<Vec<PathBuf>, String> {
    use std::ffi::{CStr, CString};
    let pattern =
        CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| "NUL in source path")?;
    let mut found: libc::glob_t = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::glob(pattern.as_ptr(), 0, None, &mut found) };
    let paths = if result == 0 {
        if found.gl_pathc > 4096 {
            Err("source-file glob exceeds path limit".into())
        } else {
            Ok((0..found.gl_pathc)
                .map(|index| unsafe {
                    os_string(CStr::from_ptr(*found.gl_pathv.add(index)).to_bytes())
                        .map(PathBuf::from)
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?)
        }
    } else if result == libc::GLOB_NOMATCH && quiet {
        Ok(Vec::new())
    } else {
        Err(format!(
            "{}: {}",
            if result == libc::GLOB_NOMATCH {
                "No such file or directory"
            } else {
                "source-file glob failed"
            },
            path.display()
        ))
    };
    unsafe {
        libc::globfree(&mut found);
    }
    paths
}

#[cfg(unix)]
pub(super) fn suitable_shell(value: &[u8]) -> bool {
    let Some(basename) = value.rsplit(|byte| *byte == b'/').next() else {
        return false;
    };
    if !value.starts_with(b"/") || matches!(basename, b"tmux" | b"tmuxx") {
        return false;
    }
    std::ffi::CString::new(value)
        .is_ok_and(|value| unsafe { libc::access(value.as_ptr(), libc::X_OK) } == 0)
}

#[cfg(windows)]
pub(super) fn suitable_shell(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    if value.contains('\0') {
        return false;
    }
    let path = Path::new(value);
    let Some(name) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
        return false;
    };
    path.is_absolute()
        && path.is_file()
        && !name.eq_ignore_ascii_case("tmux")
        && !name.eq_ignore_ascii_case("tmuxx")
        && path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com")
            })
}

#[cfg(test)]
mod file_stdin_tests {
    use super::*;

    fn host() -> (Host, Receiver<ServerMessage>) {
        let (events, _) = mpsc::sync_channel(1);
        let mut host = Host::new(events, &Options::new("unused-file-stdin"), BTreeMap::new());
        let (tx, rx) = mpsc::sync_channel(64);
        host.clients.insert(
            1,
            Client {
                tx,
                context: ClientContext::default(),
                subscribed: false,
                control: false,
                delivery_bytes: Arc::new(AtomicUsize::new(0)),
            },
        );
        host.scheduler.enable_stdin_files(1);
        (host, rx)
    }
    fn run(host: &mut Host, id: u64, arguments: &[&str]) {
        host.request(
            1,
            Request {
                request_id: id,
                argv: arguments
                    .iter()
                    .map(|arg| arg.as_bytes().to_vec())
                    .collect(),
                stdin: None,
                subscribe: false,
                control: false,
            },
        );
        drive(host);
    }
    fn drive(host: &mut Host) {
        for _ in 0..1024 {
            if !host.scheduled_work() {
                return;
            }
        }
        panic!("stdin file queue failed to quiesce");
    }
    fn requested(rx: &Receiver<ServerMessage>, request: u64) {
        assert!(rx.try_iter().any(|message|matches!(message,ServerMessage::File { request_id,path,data:None,append:false } if request_id==request && path==b"-")));
    }
    #[test]
    fn source_stdin_runs_prefix_once_and_resumes_only_on_owned_eof() {
        let (mut host, rx) = host();
        run(
            &mut host,
            7,
            &[
                "set",
                "-ag",
                "@prefix",
                "x",
                ";",
                "source-file",
                "-",
                "-",
                ";",
                "set",
                "-g",
                "@suffix",
                "after",
            ],
        );
        requested(&rx, 7);
        assert_eq!(host.engine.options["@prefix"], "x");
        assert!(!host.engine.options.contains_key("@suffix"));
        assert!(
            host.stdin_chunk(2, 7, Ok(Some(b"set -g @body forged".to_vec())))
                .is_err()
        );
        assert!(
            host.stdin_chunk(1, 8, Ok(Some(b"set -g @body forged".to_vec())))
                .is_err()
        );
        assert!(host.stdin_chunk(1, 7, Ok(Some(vec![0; 65537]))).is_err());
        for chunk in [b"set -g @body ".as_slice(), b"complete\n"] {
            host.stdin_chunk(1, 7, Ok(Some(chunk.to_vec()))).unwrap();
            drive(&mut host);
            assert!(!host.engine.options.contains_key("@body"));
            assert_eq!(host.engine.options["@prefix"], "x");
        }
        host.stdin_chunk(1, 7, Ok(None)).unwrap();
        drive(&mut host);
        assert_eq!(host.engine.options["@body"], "complete");
        assert_eq!(host.engine.options["@prefix"], "x");
        assert_eq!(host.engine.options["@suffix"], "after");
        let messages = rx.try_iter().collect::<Vec<_>>();
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, ServerMessage::File { .. })),
            "repeated stdin was requested again"
        );
        assert!(messages.iter().any(|m|matches!(m,ServerMessage::Reply(reply) if reply.request_id==7 && reply.error.as_deref()==Some("Bad file descriptor: -"))));
        assert!(host.stdin_chunk(1, 7, Ok(None)).is_err());
        assert_eq!(host.engine.options["@prefix"], "x");
    }
    #[test]
    fn buffer_stdin_preserves_every_byte_and_repeated_reads_keep_callback_order() {
        let (mut host, rx) = host();
        run(
            &mut host,
            8,
            &[
                "load-buffer",
                "-b",
                "binary",
                "-",
                ";",
                "show-buffer",
                "-b",
                "binary",
            ],
        );
        requested(&rx, 8);
        let bytes = (0..=255u8).collect::<Vec<_>>();
        for chunk in bytes.chunks(17) {
            host.stdin_chunk(1, 8, Ok(Some(chunk.to_vec()))).unwrap();
        }
        host.stdin_chunk(1, 8, Ok(None)).unwrap();
        drive(&mut host);
        assert!(rx.try_iter().any(|m|matches!(m,ServerMessage::Reply(reply) if reply.request_id==8 && reply.error.is_none() && reply.output==bytes)));
        run(
            &mut host,
            9,
            &[
                "load-buffer",
                "-b",
                "second",
                "-",
                ";",
                "set",
                "-g",
                "@after-ebadf",
                "ran",
            ],
        );
        assert_eq!(host.engine.options["@after-ebadf"], "ran");
        let messages = rx.try_iter().collect::<Vec<_>>();
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, ServerMessage::File { .. }))
        );
        assert!(messages.iter().any(|m|matches!(m,ServerMessage::Reply(reply) if reply.request_id==9 && reply.error.as_deref()==Some("Bad file descriptor: -"))));
    }
    #[test]
    fn stdin_overflow_drains_bounded_bytes_and_disconnect_discards_uncommitted_source() {
        let (mut host, rx) = host();
        run(
            &mut host,
            10,
            &["source-file", "-", ";", "set", "-g", "@cancelled", "bad"],
        );
        requested(&rx, 10);
        let mut first = b"set -g @poison bad\n#".to_vec();
        first.resize(65536, b'x');
        host.stdin_chunk(1, 10, Ok(Some(first))).unwrap();
        for _ in 0..MAX_REQUEST_BYTES / 65536 {
            host.stdin_chunk(1, 10, Ok(Some(vec![b'x'; 65536])))
                .unwrap();
            assert!(host.scheduler.stdin_files[&1].bytes.len() <= MAX_REQUEST_BYTES);
        }
        assert!(host.scheduler.stdin_files[&1].bytes.is_empty());
        assert!(host.scheduler.stdin_files[&1].error.is_some());
        host.stdin_chunk(1, 10, Ok(Some(vec![0; 65536]))).unwrap();
        host.stdin_chunk(1, 10, Ok(None)).unwrap();
        drive(&mut host);
        assert!(!host.engine.options.contains_key("@poison"));
        assert!(!host.engine.options.contains_key("@cancelled"));
        assert!(rx.try_iter().any(|m|matches!(m,ServerMessage::Reply(reply) if reply.request_id==10 && reply.error.as_deref()==Some("file input exceeds limit: -"))));
        // A fresh connection owns a fresh stdin resource; disconnect cannot
        // complete partially accumulated configuration or retain its allocation.
        let (mut fresh, fresh_rx) = self::host();
        run(&mut fresh, 11, &["source-file", "-"]);
        requested(&fresh_rx, 11);
        fresh
            .stdin_chunk(1, 11, Ok(Some(b"set -g @disconnected bad\n".to_vec())))
            .unwrap();
        fresh.disconnect(1);
        drive(&mut fresh);
        assert!(fresh.scheduler.stdin_files.is_empty());
        assert!(fresh.scheduler.stdin_consumed.is_empty());
        assert!(fresh.scheduler.stdin_clients.is_empty());
        assert!(fresh.scheduler.files.is_empty());
        assert!(!fresh.engine.options.contains_key("@disconnected"));
    }
}

#[cfg(test)]
mod source_admission_tests {
    use super::*;

    #[test]
    fn prepared_environment_rejects_malformed_values_atomically_and_keeps_ordering() {
        let (tx, _) = mpsc::sync_channel(1);
        let mut host = Host::new(
            tx,
            &Options::new("unused-source-admission"),
            BTreeMap::new(),
        );
        host.engine
            .environment
            .insert("KEEP".into(), "value�".into());
        host.engine
            .environment
            .insert("NAME�".into(), "kept".into());
        host.engine.hidden_environment.insert("KEEP".into());
        host.engine.removed_environment.insert("MISSING".into());
        let environment = host.engine.environment.clone();
        let hidden = host.engine.hidden_environment.clone();
        let removed = host.engine.removed_environment.clone();
        for input in [
            b"KEEP=changed\nBAD='\xff'\n".as_slice(),
            b"KEEP=changed\n%hidden BAD='\xff'\n",
        ] {
            let error = host
                .prepare_source(input, &ClientContext::default(), false)
                .err()
                .expect("reject malformed prepared value");
            assert!(error.contains("non-UTF8 environment values"), "{error}");
            assert_eq!(host.engine.environment, environment);
            assert_eq!(host.engine.hidden_environment, hidden);
            assert_eq!(host.engine.removed_environment, removed);
        }
        // The ASCII assignment grammar never converts this into NAME<U+FFFD>.
        let source = host
            .prepare_source(b"NAME\xff=changed", &ClientContext::default(), false)
            .unwrap();
        assert!(source.error.is_some());
        assert_eq!(host.engine.environment, environment);
        let source = host
            .prepare_source(
                b"ORDER=first\ndisplay '$ORDER'\nunknown-command\nORDER=last",
                &ClientContext::default(),
                false,
            )
            .unwrap();
        assert!(source.error.is_some());
        assert_eq!(host.engine.environment["ORDER"], "last");
        assert_eq!(source.groups.len(), 1);
        assert_eq!(host.engine.environment["NAME�"], "kept");
        host.prepare_source(b"KEEP='\xff'", &ClientContext::default(), true)
            .unwrap();
        assert_eq!(host.engine.environment["KEEP"], "value�");
    }
}
