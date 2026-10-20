use super::*;

pub(super) struct FormatProcess {
    request: tmuxx_core::FormatJobRequest,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl FormatProcess {
    pub(super) fn command(&self) -> &[u8] {
        &self.request.command
    }
}
impl Drop for FormatProcess {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Host {
    pub(super) fn start_format_jobs(&mut self) {
        for request in self.engine.take_format_jobs() {
            if let Some(replaced) = request.replaced {
                self.format_jobs
                    .retain(|_, process| process.request.id != replaced);
            }
            if self.format_jobs.len() >= 128 {
                self.engine.format_job_output(request.id, &[], true, true);
                continue;
            }
            let job_id = self.scheduler.next_job_id();
            let stop = Arc::new(AtomicBool::new(false));
            let cwd = request
                .cwd
                .as_ref()
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .or_else(|| {
                    self.engine
                        .environment
                        .get("HOME")
                        .map(PathBuf::from)
                        .filter(|path| path.is_dir())
                })
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| "/".into());
            let mut environment = self
                .engine
                .environment
                .iter()
                .filter(|(key, _)| {
                    !self.engine.hidden_environment.contains(*key)
                        && !self.engine.removed_environment.contains(*key)
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>();
            let Some(cwd_text) = cwd.to_str() else {
                self.engine.format_job_output(request.id, &[], true, true);
                continue;
            };
            environment.insert("PWD".into(), cwd_text.into());
            environment.insert(
                "TERM".into(),
                self.engine
                    .server_options
                    .get("default-terminal")
                    .cloned()
                    .unwrap_or_else(|| "tmux-256color".into()),
            );
            if let Some(endpoint) = &self.tmux_endpoint {
                environment.insert(
                    "TMUX".into(),
                    format!("{},{},-1", endpoint.display(), std::process::id()),
                );
            }
            match scheduler::spawn_job(
                job_id,
                request.command.clone(),
                super::defaults::system_shell(),
                cwd,
                environment,
                Duration::ZERO,
                false,
                false,
                true,
                stop.clone(),
                self.tx.clone(),
            ) {
                Ok(thread) => {
                    self.format_jobs.insert(
                        job_id,
                        FormatProcess {
                            request,
                            stop,
                            thread: Some(thread),
                        },
                    );
                }
                Err(_) => self.engine.format_job_output(request.id, &[], true, true),
            }
        }
    }

    pub(super) fn format_job_line(&mut self, id: u64, data: Vec<u8>) {
        if let Some(process) = self.format_jobs.get(&id) {
            self.engine
                .format_job_output(process.request.id, &data, false, false);
        }
    }

    pub(super) fn format_job_finished(&mut self, id: u64, output: &[u8], failed: bool) -> bool {
        let Some(process) = self.format_jobs.remove(&id) else {
            return false;
        };
        self.engine
            .format_job_output(process.request.id, output, true, failed);
        true
    }

    pub(super) fn disconnect_format_jobs(&mut self, client: &str) {
        self.format_jobs
            .retain(|_, process| process.request.client != client);
        self.engine.forget_format_jobs(client);
    }
}
