//! Local providers are transient; only targeted native commands carry their values.
use super::*;
use std::time::Instant;

const OUTPUT_LIMIT: usize = 64 * 1024;
const TOTAL_LIMIT: usize = 1024 * 1024;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(30);
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

enum SessionState {
    Unprepared,
    Pending {
        reply: mpsc::Receiver<io::Result<Vec<u8>>>,
        installing: bool,
        deadline: Instant,
    },
    Cleared,
    Ready,
    Failed,
}

/// Resolves once per connection and installs values before its session views open.
pub struct SessionEnvironment {
    names: Vec<String>,
    receiver: mpsc::Receiver<io::Result<BTreeMap<String, String>>>,
    values: Option<io::Result<BTreeMap<String, String>>>,
    sessions: BTreeMap<u64, SessionState>,
    cancel: Arc<AtomicBool>,
}

impl SessionEnvironment {
    pub fn start(
        commands: &BTreeMap<String, String>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        if commands.iter().any(|(name, command)| {
            !crate::config::environment_name(name)
                || command.trim().is_empty()
                || command.contains('\0')
        }) {
            return Err(err("invalid SSH environment provider"));
        }
        let names = commands.keys().cloned().collect();
        let commands = commands.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("rustty-ssh-environment".into())
            .spawn(move || {
                let result = (|| {
                    let deadline = Instant::now() + PROVIDER_TIMEOUT;
                    let mut values = BTreeMap::new();
                    let mut total = 0;
                    for (name, command) in commands {
                        let value = resolve(&command, deadline, &stopped).map_err(|reason| {
                            err(format!("SSH environment provider {name}: {reason}"))
                        })?;
                        total += value.len();
                        if total > TOTAL_LIMIT {
                            return Err(err("SSH environment providers exceed 1 MiB"));
                        }
                        values.insert(name, value);
                    }
                    Ok(values)
                })();
                let _ = sender.send(result);
                wake();
            })?;
        Ok(Self {
            names,
            receiver,
            values: None,
            sessions: BTreeMap::new(),
            cancel,
        })
    }

    pub fn ready(&self, sessions: &std::collections::BTreeSet<u64>) -> bool {
        self.values.as_ref().is_some_and(Result::is_ok)
            && sessions
                .iter()
                .all(|id| matches!(self.sessions.get(id), Some(SessionState::Ready)))
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.sessions
            .values()
            .filter_map(|state| match state {
                SessionState::Pending { deadline, .. } => Some(*deadline),
                _ => None,
            })
            .min()
    }

    /// Poll acknowledgments; clear previous exports even if a provider fails.
    pub fn prepare(
        &mut self,
        connection: &Connection,
        selected: &std::collections::BTreeSet<u64>,
    ) -> io::Result<bool> {
        self.sessions
            .retain(|session, _| selected.contains(session));
        if self.values.is_none() {
            match self.receiver.try_recv() {
                Ok(values) => self.values = Some(values),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.values = Some(Err(err("SSH environment providers stopped")));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut failed = false;
        for &session in selected {
            let state = self
                .sessions
                .entry(session)
                .or_insert(SessionState::Unprepared);
            if let SessionState::Pending {
                reply,
                installing,
                deadline,
            } = state
            {
                match reply.try_recv() {
                    Ok(Ok(_)) => {
                        *state = if *installing {
                            SessionState::Ready
                        } else {
                            SessionState::Cleared
                        }
                    }
                    Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                        *state = SessionState::Failed
                    }
                    Err(mpsc::TryRecvError::Empty) if Instant::now() >= *deadline => {
                        *state = SessionState::Failed
                    }
                    Err(mpsc::TryRecvError::Empty) => continue,
                }
            }
            if matches!(state, SessionState::Failed) {
                failed = true;
                continue;
            }
            if matches!(state, SessionState::Ready) {
                continue;
            }
            // The first request hides previous session and global values. Readiness
            // requires a second, acknowledged request installing the fresh values.
            let installing = matches!(state, SessionState::Cleared);
            let values = match &self.values {
                Some(Ok(values)) => Some(values),
                _ => None,
            };
            if installing && values.is_none() {
                continue;
            }
            let argv =
                environment_commands(session, &self.names, if installing { values } else { None });
            let reply = connection.command_with_reply(argv).map_err(|_| {
                err("Could not send the remote session environment; reconnect to retry")
            })?;
            self.sessions.insert(
                session,
                SessionState::Pending {
                    reply,
                    installing,
                    deadline: Instant::now() + REPLY_TIMEOUT,
                },
            );
        }
        if failed {
            return Err(err(
                "Could not update the remote session environment; reconnect to retry",
            ));
        }
        if let Some(Err(error)) = &self.values {
            return Err(err(error));
        }
        Ok(self.ready(selected))
    }
}

impl Drop for SessionEnvironment {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn environment_commands(
    session: u64,
    names: &[String],
    values: Option<&BTreeMap<String, String>>,
) -> Vec<Vec<u8>> {
    let mut argv = Vec::new();
    for name in names {
        if !argv.is_empty() {
            argv.push(b";".to_vec());
        }
        argv.push(b"set-environment".to_vec());
        if values.is_none() {
            argv.push(b"-r".to_vec());
        }
        argv.extend([
            b"-t".to_vec(),
            format!("${session}").into_bytes(),
            name.as_bytes().to_vec(),
        ]);
        if let Some(values) = values {
            let mut value = values[name].as_bytes().to_vec();
            // Native argv treats a final semicolon as a command separator.
            if value.last() == Some(&b';') {
                value.insert(value.len() - 1, b'\\');
            }
            argv.push(value);
        }
    }
    argv
}

struct ProviderChild {
    #[cfg(unix)]
    child: Child,
    #[cfg(unix)]
    reaped: bool,
    #[cfg(windows)]
    child: tmuxx_runtime::OwnedStdioChild,
}
impl Drop for ProviderChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            // The provider has its own process group, including background helpers.
            let pid = self.child.id() as libc::pid_t;
            if !self.reaped || libc::getpgid(pid) < 0 {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn resolve(command: &str, deadline: Instant, cancel: &AtomicBool) -> Result<String, &'static str> {
    if cancel.load(Ordering::Acquire) {
        return Err("canceled");
    }
    if Instant::now() >= deadline {
        return Err("timed out after 30 seconds");
    }
    #[cfg(unix)]
    let mut child = {
        use std::os::unix::process::CommandExt;
        ProviderChild {
            child: std::process::Command::new("/bin/sh")
                .args(["-c", command])
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| "could not start local shell")?,
            reaped: false,
        }
    };
    #[cfg(windows)]
    let mut child = {
        let shell = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .ok_or("SystemRoot is missing")?
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let source = format!(
            "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false); $ErrorActionPreference = 'Stop'; {command}; if ($LASTEXITCODE) {{ exit $LASTEXITCODE }}"
        );
        let mut child = tmuxx_runtime::spawn_owned_stdio_piped_stderr(
            &shell,
            &[
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                source.into(),
            ],
        )
        .map_err(|_| "could not start local shell")?;
        child.stdin.take();
        let stderr = child.stderr.take().unwrap();
        thread::Builder::new()
            .name("rustty-env-stderr".into())
            .spawn(move || {
                let _ = io::copy(&mut BufReader::new(stderr), &mut io::sink());
            })
            .map_err(|_| "could not read local shell output")?;
        ProviderChild { child }
    };
    let stdout = child.child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("rustty-env-stdout".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take((OUTPUT_LIMIT + 1) as u64)
                .read_to_end(&mut bytes);
            let _ = sender.send(result.map(|_| bytes));
        })
        .map_err(|_| "could not read local shell output")?;
    let mut output = None;
    let mut status = None;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("canceled");
        }
        if Instant::now() >= deadline {
            return Err("timed out after 30 seconds");
        }
        if output.is_none() {
            match receiver.try_recv() {
                Ok(Ok(bytes)) if bytes.len() <= OUTPUT_LIMIT => output = Some(bytes),
                Ok(Ok(_)) => return Err("output exceeds 64 KiB"),
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("could not read output");
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if status.is_none() {
            status = child
                .child
                .try_wait()
                .map_err(|_| "could not wait for local shell")?;
            #[cfg(unix)]
            {
                child.reaped = status.is_some();
            }
        }
        if let Some(status) = status {
            if !status.success() {
                return Err("command failed");
            }
            if let Some(bytes) = output {
                let value = String::from_utf8(bytes).map_err(|_| "output is not UTF-8")?;
                if value.contains('\0') {
                    return Err("output contains NUL");
                }
                let mut value = value.as_str();
                while let Some(rest) = value.strip_suffix('\n') {
                    value = rest.strip_suffix('\r').unwrap_or(rest);
                }
                return Ok(value.to_owned());
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn fixture(values: io::Result<BTreeMap<String, String>>) -> SessionEnvironment {
        let (sender, receiver) = mpsc::channel();
        sender.send(values).unwrap();
        SessionEnvironment {
            names: vec!["TOKEN".into()],
            receiver,
            values: None,
            sessions: BTreeMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
    fn reply(connection: &Connection, request: &Request, error: Option<&str>) {
        apply(
            &connection.shared,
            ServerMessage::Reply(tmuxx_protocol::native::Response {
                request_id: request.request_id,
                status: None,
                output: Vec::new(),
                error: error.map(str::to_owned),
            }),
        )
        .unwrap();
    }

    #[test]
    fn session_environment_waits_for_clear_and_install_acknowledgments() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let value = " spaces \"'$\\\n;";
        let mut environment = fixture(Ok(BTreeMap::from([("TOKEN".into(), value.into())])));
        let mut selected = BTreeSet::from([7]);
        assert!(!environment.prepare(&connection, &selected).unwrap());
        let clear = requests.try_recv().unwrap();
        assert_eq!(
            clear.argv,
            ["set-environment", "-r", "-t", "$7", "TOKEN"].map(|s| s.as_bytes().to_vec())
        );
        assert!(!environment.prepare(&connection, &selected).unwrap());
        assert!(requests.try_recv().is_err());
        reply(&connection, &clear, None);
        assert!(!environment.prepare(&connection, &selected).unwrap());
        let install = requests.try_recv().unwrap();
        assert_eq!(
            tmuxx_command::parse_argv(&install.argv).unwrap(),
            vec![tmuxx_command::Command::new(
                ["set-environment", "-t", "$7", "TOKEN", value]
                    .map(|s| s.as_bytes().to_vec())
                    .to_vec()
            )]
        );
        assert!(!environment.ready(&selected));
        reply(&connection, &install, None);
        assert!(environment.prepare(&connection, &selected).unwrap());
        assert!(requests.try_recv().is_err());
        selected.insert(8);
        assert!(!environment.ready(&selected));
        assert!(!environment.prepare(&connection, &selected).unwrap());
        let clear = requests.try_recv().unwrap();
        assert_eq!(clear.argv[3], b"$8");
        assert!(
            requests.try_recv().is_err(),
            "already prepared session was updated again"
        );
    }

    #[test]
    fn session_environment_failure_clears_stale_values_and_hides_reply_output() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let selected = BTreeSet::from([7, 8]);
        let mut failed = fixture(Err(err("SSH environment provider TOKEN: command failed")));
        assert!(failed.prepare(&connection, &selected).is_err());
        for _ in 0..2 {
            let clear = requests.try_recv().unwrap();
            assert_eq!(clear.argv[1], b"-r");
            reply(&connection, &clear, None);
        }
        assert!(failed.prepare(&connection, &selected).is_err());
        assert!(!failed.ready(&selected));
        assert!(requests.try_recv().is_err());

        let mut environment = fixture(Ok(BTreeMap::from([(
            "TOKEN".into(),
            "private-value".into(),
        )])));
        assert!(!environment.prepare(&connection, &selected).unwrap());
        for request in requests.try_iter() {
            reply(&connection, &request, None);
        }
        assert!(!environment.prepare(&connection, &selected).unwrap());
        for request in requests.try_iter() {
            reply(
                &connection,
                &request,
                Some("private-value in remote failure"),
            );
        }
        let error = environment
            .prepare(&connection, &selected)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("private-value"));
        assert!(connection.errors().is_empty());
        assert!(!environment.ready(&selected));
    }

    #[test]
    fn session_environment_reply_timeout_blocks_readiness() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let selected = BTreeSet::from([7]);
        let mut environment = fixture(Ok(BTreeMap::from([("TOKEN".into(), "value".into())])));
        assert!(!environment.prepare(&connection, &selected).unwrap());
        if let SessionState::Pending { deadline, .. } = environment.sessions.get_mut(&7).unwrap() {
            *deadline = Instant::now();
        }
        assert!(environment.prepare(&connection, &selected).is_err());
        assert!(!environment.ready(&selected));
        assert!(environment.deadline().is_none());
    }

    #[test]
    fn session_environment_preserves_literal_semicolons_and_multiple_variables() {
        for value in [
            "",
            ";",
            "\\;",
            "two;commands;",
            "\"quoted\"",
            "line\nline",
            " leading and trailing ",
            "${HOME}",
        ] {
            let values = BTreeMap::from([("A".into(), value.into()), ("B".into(), value.into())]);
            let argv = environment_commands(7, &["A".into(), "B".into()], Some(&values));
            let commands = tmuxx_command::parse_argv(&argv).unwrap();
            assert_eq!(commands.len(), 2);
            for command in commands {
                assert_eq!(command.args.last().unwrap(), value.as_bytes());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn ssh_environment_provider_bounds_the_batch_and_cancels_on_drop() {
        let commands = (0..17)
            .map(|index| {
                (
                    format!("TOKEN_{index}"),
                    "head -c 65536 /dev/zero | tr '\\000' x".into(),
                )
            })
            .collect();
        let environment = SessionEnvironment::start(&commands, Arc::new(|| {})).unwrap();
        let error = environment
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), "SSH environment providers exceed 1 MiB");
        let (finished, receiver) = mpsc::channel();
        let environment = SessionEnvironment::start(
            &BTreeMap::from([("TOKEN".into(), "sleep 30".into())]),
            Arc::new(move || {
                let _ = finished.send(());
            }),
        )
        .unwrap();
        drop(environment);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ssh_environment_provider_validates_output_and_sanitizes_failures() {
        let cancel = AtomicBool::new(false);
        for (command, expected) in [
            ("printf ' a=b \\n\\n'", Ok(" a=b ")),
            ("printf 'line\\nline\\r\\n'", Ok("line\nline")),
            ("printf ''", Ok("")),
            ("printf '\\377'", Err("output is not UTF-8")),
            ("printf '\\000'", Err("output contains NUL")),
            ("head -c 65537 /dev/zero", Err("output exceeds 64 KiB")),
            (
                "printf private-value; printf private-error >&2; exit 1",
                Err("command failed"),
            ),
        ] {
            let result = resolve(command, Instant::now() + Duration::from_secs(5), &cancel);
            assert_eq!(
                result.as_deref().map_err(|error| *error),
                expected,
                "{command}"
            );
        }
        let until = Instant::now() + Duration::from_millis(100);
        assert_eq!(
            resolve("sleep 30 & wait", until, &cancel),
            Err("timed out after 30 seconds")
        );
        assert!(Instant::now() < until + Duration::from_secs(1));
        cancel.store(true, Ordering::Release);
        assert_eq!(
            resolve("sleep 30", Instant::now() + PROVIDER_TIMEOUT, &cancel),
            Err("canceled")
        );
    }
}
