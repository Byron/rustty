//! Streaming pipe ownership and a single output route for PTYs and empty panes.
use super::*;

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn session_groups(pid: Option<u32>, reaped: bool) -> Vec<Option<libc::pid_t>> {
    let Some(session) = pid.map(|pid| pid as libc::pid_t) else {
        return Vec::new();
    };
    // A reaped leader cannot still exist. If its PID now exists, it belongs to
    // a later process and must never become a cleanup target.
    if reaped && unsafe { libc::getsid(session) } >= 0 {
        return Vec::new();
    }
    let mut groups = std::collections::BTreeSet::from([session]);
    #[cfg(target_os = "macos")]
    let processes = {
        // A bounded PID inventory; inspect session IDs only, never process data.
        let mut ids = vec![0 as libc::pid_t; 131072];
        let count = unsafe {
            libc::proc_listallpids(
                ids.as_mut_ptr().cast(),
                std::mem::size_of_val(ids.as_slice()) as libc::c_int,
            )
        };
        ids.truncate(count.max(0) as usize);
        ids
    };
    #[cfg(target_os = "linux")]
    let processes = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            entry
                .ok()?
                .file_name()
                .to_str()?
                .parse::<libc::pid_t>()
                .ok()
        })
        .take(131072)
        .collect::<Vec<_>>();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let processes = Vec::<libc::pid_t>::new();
    for pid in processes {
        // Every PTY spawn calls setsid. This excludes the application and all
        // independent hosts, and still finds orphaned jobs in other groups.
        if pid > 0 && unsafe { libc::getsid(pid) } == session {
            let group = unsafe { libc::getpgid(pid) };
            if group > 0 {
                groups.insert(group);
            }
        }
    }
    groups.into_iter().map(Some).collect()
}

#[cfg(unix)]
pub(super) fn signal_groups(groups: &[Option<libc::pid_t>], signal: libc::c_int) {
    for group in groups.iter().flatten().copied() {
        // The PTY child creates a new session; the other group is read from
        // that PTY's foreground state. Never target the application's group.
        if group > 0 && group != unsafe { libc::getpgrp() } {
            unsafe {
                libc::kill(-group, signal);
            }
        }
    }
}

pub(super) fn exit_status(status: portable_pty::ExitStatus) -> (i32, Option<String>) {
    #[cfg(unix)]
    if let Some(name) = status.signal() {
        // portable-pty retains strsignal's spelling rather than the native
        // signal number. Recover that number before exposing shell status.
        for signal in 1..128 {
            // SAFETY: strsignal returns a NUL-terminated diagnostic for an integer.
            let spelling = unsafe { libc::strsignal(signal) };
            if !spelling.is_null()
                && unsafe { std::ffi::CStr::from_ptr(spelling) }.to_bytes() == name.as_bytes()
            {
                return (128 + signal, Some(signal_name(signal)));
            }
        }
    }
    (
        status.exit_code() as i32,
        status.signal().map(str::to_owned),
    )
}

#[cfg(unix)]
fn signal_name(signal: libc::c_int) -> String {
    #[cfg(target_os = "macos")]
    if (1..32).contains(&signal) {
        unsafe extern "C" {
            static sys_signame: [*const libc::c_char; 32];
        }
        // SAFETY: Darwin exports one static signal name for each index 0..31.
        return unsafe { std::ffi::CStr::from_ptr(sys_signame[signal as usize]) }
            .to_string_lossy()
            .into_owned();
    }
    signal.to_string()
}
pub(super) struct StdinStream {
    pub(super) pane: Id,
    pub(super) request_id: u64,
    pub(super) group: u64,
}

pub(super) struct PipeProcess {
    #[cfg(windows)]
    _ownership: Arc<super::windows_job::Job>,
    pid: u32,
    pub(super) generation: u64,
    pub(super) input: bool,
    output: Option<mpsc::Sender<Vec<u8>>>,
    bytes: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    stop: SyncSender<()>,
    finished: Receiver<()>,
    reaped: bool,
}
impl PipeProcess {
    pub(super) fn pending_bytes(&self) -> usize {
        self.bytes.load(Ordering::Acquire)
    }
}
impl Drop for PipeProcess {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.output.take();
        let _ = self.stop.try_send(());
        if !self.reaped {
            let _ = self.finished.recv_timeout(Duration::from_secs(2));
        }
        #[cfg(unix)]
        signal_groups(&[Some(self.pid as libc::pid_t)], libc::SIGKILL);
    }
}

impl Host {
    pub(super) fn stdin_chunk(
        &mut self,
        id: u64,
        request_id: u64,
        data: Result<Option<Vec<u8>>, String>,
    ) -> Result<(), String> {
        if self.scheduler_has_file_stdin(id) {
            return self.file_stdin_chunk(id, request_id, data).unwrap();
        }
        let stream = self
            .stdin_streams
            .get(&id)
            .filter(|stream| stream.request_id == request_id)
            .ok_or("no pending stdin request")?;
        let pane = stream.pane;
        if !self.engine.panes.contains_key(&pane) {
            self.stdin_complete(id, Some("pane has been removed".into()));
            return Err("pane has been removed".into());
        }
        match data {
            Ok(Some(bytes)) if bytes.len() <= 65536 => {
                if !bytes.is_empty() {
                    self.pane_output(pane, &bytes);
                }
            }
            Ok(Some(_)) => return Err("stdin chunk exceeds limit".into()),
            Ok(None) => self.stdin_complete(id, None),
            Err(error) => self.stdin_complete(id, Some(error)),
        }
        Ok(())
    }
    pub(super) fn reap_pipes(&mut self) {
        self.retired_pipes.retain_mut(|pipe| {
            pipe.reaped = !matches!(pipe.finished.try_recv(), Err(mpsc::TryRecvError::Empty));
            !pipe.reaped
        });
    }
    pub(super) fn close_pipe(&mut self, pane: Id) {
        if let Some(mut pipe) = self.pipes.remove(&pane) {
            pipe.cancel.store(true, Ordering::Release);
            pipe.output.take();
            self.retired_pipes.push(pipe);
        }
        if let Some(pane) = self.engine.panes.get_mut(&pane) {
            pane.pipe = false;
            pane.pipe_pid = None;
        }
    }

    pub(super) fn pipe(
        &mut self,
        pane: Id,
        command: Option<Vec<u8>>,
        input: bool,
        output: bool,
    ) -> Result<(), String> {
        self.close_pipe(pane);
        let Some(command) = command else {
            return Ok(());
        };
        self.reap_pipes();
        if self.retired_pipes.len() >= 256 {
            return Err("pipe child limit exceeded".into());
        }
        #[cfg(unix)]
        let mut child = {
            let mut builder = std::process::Command::new("/bin/sh");
            builder
                .arg("-c")
                .arg(os_string(&command).map_err(|error| error.to_string())?)
                .stdin(if output {
                    std::process::Stdio::piped()
                } else {
                    std::process::Stdio::null()
                })
                .stdout(if input {
                    std::process::Stdio::piped()
                } else {
                    std::process::Stdio::null()
                })
                .stderr(std::process::Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                builder.process_group(0);
            }
            builder.spawn().map_err(|error| error.to_string())?
        };
        #[cfg(windows)]
        let mut child = {
            let cwd = self
                .engine
                .panes
                .get(&pane)
                .and_then(|pane| pane.cwd.as_deref())
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .ok_or("pipe working directory unavailable")?;
            super::windows_job::spawn_shell(
                &super::defaults::system_shell(),
                &command,
                &cwd,
                &self.engine.environment,
                output,
                input,
                false,
            )
            .map_err(|error| error.to_string())?
        };
        let writer = child.stdin.take();
        let reader = child.stdout.take();
        let generation = self.next_process;
        self.next_process += 1;
        // The byte counter includes message overhead, so short PTY reads do not
        // overflow an arbitrary message-count limit while staying within budget.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (stop, stop_rx) = mpsc::sync_channel(1);
        let (done, finished) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicUsize::new(0));
        let process = PipeProcess {
            #[cfg(windows)]
            _ownership: child.ownership(),
            pid: child.id(),
            generation,
            input,
            output: output.then_some(tx),
            stop,
            finished,
            cancel: cancel.clone(),
            bytes: bytes.clone(),
            reaped: false,
        };
        let child = Arc::new(Mutex::new(child));
        let waiter = child.clone();
        let events = self.tx.clone();
        let reaper = thread::Builder::new()
            .name(format!("tmuxx-pipe-wait-{pane}"))
            .spawn(move || {
                let mut child = waiter.lock().unwrap();
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => {}
                    }
                    if !matches!(
                        stop_rx.recv_timeout(Duration::from_millis(10)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        #[cfg(unix)]
                        {
                            // SAFETY: this child was placed in a new process group before exec.
                            unsafe {
                                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                            }
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
                let _ = done.send(());
                let _ = events.send(Event::PipeExited(pane, generation));
            });
        if let Err(error) = reaper {
            let mut child = child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        if let Some(mut writer) = writer {
            let events = self.tx.clone();
            thread::Builder::new()
                .name(format!("tmuxx-pipe-write-{pane}"))
                .spawn(move || {
                    for data in rx {
                        if cancel.load(Ordering::Acquire) {
                            break;
                        }
                        let result = writer.write_all(&data);
                        let cost = data.len() + 128;
                        let left = bytes.fetch_sub(cost, Ordering::AcqRel) - cost;
                        if result.is_err() {
                            let _ = events.send(Event::PipeClosed(pane, generation));
                            break;
                        }
                        if left == 0 {
                            let _ = events.send(Event::PipeDrained(pane));
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
        }
        if let Some(mut reader) = reader {
            let events = self.tx.clone();
            thread::Builder::new()
                .name(format!("tmuxx-pipe-read-{pane}"))
                .spawn(move || {
                    let mut data = [0; 32768];
                    loop {
                        match reader.read(&mut data) {
                            Ok(0) => break,
                            Ok(n) => {
                                if events
                                    .send(Event::PipeInput(pane, generation, data[..n].to_vec()))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                                ) =>
                            {
                                thread::sleep(Duration::from_millis(1));
                                continue;
                            }
                            Err(_) => break,
                        }
                    }
                    let _ = events.send(Event::PipeClosed(pane, generation));
                })
                .map_err(|error| error.to_string())?;
        }
        let pid = process.pid;
        self.pipes.insert(pane, process);
        if let Some(pane) = self.engine.panes.get_mut(&pane) {
            pane.pipe = true;
            pane.pipe_pid = Some(pid);
        }
        Ok(())
    }

    pub(super) fn pane_output(&mut self, pane: Id, data: &[u8]) {
        if !self.engine.panes.contains_key(&pane) {
            return;
        }
        if let Some(pipe) = self.pipes.get(&pane)
            && let Some(output) = &pipe.output
        {
            let cost = data.len() + 128;
            let total = pipe
                .bytes
                .fetch_add(cost, Ordering::AcqRel)
                .saturating_add(cost);
            if total > MAX_REQUEST_BYTES || output.send(data.to_vec()).is_err() {
                pipe.bytes.fetch_sub(cost, Ordering::AcqRel);
                self.close_pipe(pane);
                eprintln!("tmuxx: pane %{pane} pipe output queue unavailable");
            }
        }
        let before = self.hook_snapshot();
        let graphics_before = self
            .engine
            .panes
            .iter()
            .map(|(&id, pane)| (id, pane.terminal.graphics_generation()))
            .collect::<BTreeMap<_, _>>();
        let effects = self.engine.feed(pane, data);
        let graphics_changed = self.engine.panes.iter().any(|(id, pane)| {
            graphics_before.get(id) != Some(&pane.terminal.graphics_generation())
        });
        if let Some(before) = before {
            self.graph_hooks(before, false);
        }
        if let Err(error) = self.effects(effects) {
            self.input_failed(pane, error);
        }
        let sequence = self.engine.sequence;
        let clients = self
            .clients
            .iter()
            .filter(|(_, client)| {
                client.subscribed
                    && (!client.control || self.pane_in_session(pane, client.context.session))
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in clients {
            self.send(
                id,
                ServerMessage::PaneOutput {
                    sequence,
                    pane_id: pane,
                    data: data.to_vec(),
                },
            );
            if graphics_changed && self.clients.get(&id).is_some_and(|client| !client.control) {
                self.send(id, ServerMessage::Changed { sequence });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pipe_overflow_closes_pipe_without_losing_or_duplicating_terminal_output() {
        let (events, _) = mpsc::sync_channel(1);
        let mut host = Host::new(events, &Options::new("unused-pipe-test"), BTreeMap::new());
        host.engine
            .execute(
                &tmuxx_command::Command {
                    blocks: Default::default(),
                    args: ["new-session", "-d"]
                        .map(|word| word.as_bytes().to_vec())
                        .to_vec(),
                },
                &mut ClientContext::default(),
            )
            .unwrap();
        let (output, received) = mpsc::channel();
        let (stop, stopped) = mpsc::sync_channel(1);
        let (done, finished) = mpsc::sync_channel(1);
        host.pipes.insert(
            0,
            PipeProcess {
                #[cfg(windows)]
                _ownership: super::super::windows_job::Job::new().unwrap(),
                pid: 0,
                generation: 1,
                input: false,
                output: Some(output),
                bytes: Arc::new(AtomicUsize::new(MAX_REQUEST_BYTES)),
                cancel: Arc::new(AtomicBool::new(false)),
                stop,
                finished,
                reaped: false,
            },
        );
        host.engine.panes.get_mut(&0).unwrap().pipe = true;
        host.pane_output(0, b"visible-once");
        assert!(!host.pipes.contains_key(&0));
        assert!(!host.engine.panes[&0].pipe);
        assert!(received.try_recv().is_err());
        assert!(
            stopped.try_recv().is_err(),
            "normal pipe close must allow the child to drain"
        );
        assert_eq!(
            host.engine.panes[&0]
                .terminal
                .capture_lines(Some(0), Some(0), false),
            "visible-once\n"
        );
        done.send(()).unwrap();
        host.reap_pipes();
        assert!(host.retired_pipes.is_empty());
    }
    #[test]
    fn stdin_chunks_enforce_ownership_and_resume_only_after_eof() {
        let (events, _) = mpsc::sync_channel(1);
        let mut host = Host::new(events, &Options::new("unused-stdin-test"), BTreeMap::new());
        host.engine
            .execute(
                &tmuxx_command::Command {
                    blocks: Default::default(),
                    args: ["new-session", "-d", ""]
                        .map(|word| word.as_bytes().to_vec())
                        .to_vec(),
                },
                &mut ClientContext::default(),
            )
            .unwrap();
        host.engine.panes.get_mut(&0).unwrap().empty = true;
        let (tx, rx) = mpsc::sync_channel(32);
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
        let request = Request {
            request_id: 7,
            argv: [
                "display-message",
                "-I",
                "-t",
                "%0",
                ";",
                "set-option",
                "-g",
                "@after-input",
                "done",
            ]
            .map(|word| word.as_bytes().to_vec())
            .to_vec(),
            stdin: None,
            subscribe: false,
            control: false,
        };
        host.request(1, request.clone());
        while host.scheduled_work() {}
        assert!(rx.try_iter().any(|message| matches!(
            message,
            ServerMessage::Stdin {
                request_id: 7,
                pane_id: 0
            }
        )));
        assert!(!host.engine.options.contains_key("@after-input"));
        assert!(
            host.stdin_chunk(2, 7, Ok(Some(b"wrong-client".to_vec())))
                .is_err()
        );
        assert!(
            host.stdin_chunk(1, 8, Ok(Some(b"wrong-request".to_vec())))
                .is_err()
        );
        assert!(host.stdin_chunk(1, 7, Ok(Some(vec![0; 65537]))).is_err());
        host.stdin_chunk(1, 7, Ok(Some(b"visible-before-eof".to_vec())))
            .unwrap();
        assert_eq!(
            host.engine.panes[&0]
                .terminal
                .capture_lines(Some(0), Some(0), false),
            "visible-before-eof\n"
        );
        assert!(!host.scheduled_work());
        host.stdin_chunk(1, 7, Ok(None)).unwrap();
        assert!(host.stdin_chunk(1, 7, Ok(None)).is_err());
        while host.scheduled_work() {}
        assert_eq!(
            host.engine.options.get("@after-input").map(String::as_str),
            Some("done")
        );
        assert!(rx.try_iter().any(|message| matches!(message, ServerMessage::Reply(reply) if reply.request_id == 7 && reply.error.is_none())));
        host.engine.options.remove("@after-input");
        host.request(1, request);
        while host.scheduled_work() {}
        host.disconnect(1);
        assert!(host.stdin_streams.is_empty());
        assert!(!host.scheduled_work());
        assert!(!host.engine.options.contains_key("@after-input"));
    }
}
