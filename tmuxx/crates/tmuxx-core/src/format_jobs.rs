use super::*;

#[derive(Clone)]
pub struct FormatJobRequest {
    pub id: u64,
    pub client: String,
    pub command: Vec<u8>,
    pub cwd: Option<String>,
    pub replaced: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expand(engine: &Engine, context: &ClientContext, raw: &[u8]) -> Result<Vec<u8>, String> {
        engine.expand(raw, None, None, None, context)
    }
    #[test]
    fn job_effects_are_scoped_cancel_stale_results_and_bound_the_cache() {
        let mut engine = Engine::default();
        let first = ClientContext {
            client_name: "first".into(),
            ..ClientContext::default()
        };
        let second = ClientContext {
            client_name: "second".into(),
            ..ClientContext::default()
        };
        assert!(expand(&engine, &first, b"#(printf hello)").is_err());
        engine.enable_format_jobs();
        assert_eq!(expand(&engine, &first, b"#(printf hello)").unwrap(), b"");
        let request = engine.take_format_jobs().pop().unwrap();
        engine.format_job_output(request.id, b"hello", true, false);
        assert_eq!(
            expand(&engine, &first, b"#(printf hello)").unwrap(),
            b"hello"
        );
        assert_eq!(expand(&engine, &second, b"#(printf hello)").unwrap(), b"");
        assert_eq!(engine.take_format_jobs().len(), 1);
        engine.forget_format_jobs("first");
        engine.format_job_output(request.id, b"stale", true, false);
        assert_eq!(expand(&engine, &first, b"#(printf hello)").unwrap(), b"");
        let mut disabled = first.clone();
        disabled.no_format_jobs = true;
        assert_eq!(expand(&engine, &disabled, b"#(ignored)").unwrap(), b"");
        assert_eq!(engine.take_format_jobs().len(), 1);
        for id in 0..4094 {
            expand(&engine, &first, format!("#(printf {id})").as_bytes()).unwrap();
        }
        assert!(
            expand(&engine, &first, b"#(one too many)")
                .unwrap_err()
                .contains("cache exceeds")
        );
    }
}

#[derive(Clone)]
struct Entry {
    id: u64,
    expanded: Vec<u8>,
    output: Option<Vec<u8>>,
    started: u64,
    touched: u64,
    running: bool,
}

#[derive(Clone, Default)]
pub(super) struct FormatJobs {
    enabled: bool,
    next: u64,
    entries: BTreeMap<(String, Vec<u8>), Entry>,
    pending: Vec<FormatJobRequest>,
}

impl Engine {
    pub fn enable_format_jobs(&mut self) {
        self.format_jobs.get_mut().enabled = true;
    }

    pub fn take_format_jobs(&mut self) -> Vec<FormatJobRequest> {
        let jobs = self.format_jobs.get_mut();
        jobs.entries.retain(|_, entry| {
            self.now.saturating_sub(entry.touched) < 3_600_000_000 || entry.running
        });
        std::mem::take(&mut jobs.pending)
    }

    pub fn forget_format_jobs(&mut self, client: &str) {
        let jobs = self.format_jobs.get_mut();
        jobs.entries.retain(|(name, _), _| name != client);
        jobs.pending.retain(|job| job.client != client);
    }

    pub fn format_job_output(&mut self, id: u64, output: &[u8], complete: bool, failed: bool) {
        let jobs = self.format_jobs.get_mut();
        let Some(((_, command), entry)) = jobs.entries.iter_mut().find(|(_, entry)| entry.id == id)
        else {
            return;
        };
        if !complete || !output.is_empty() || entry.output.is_none() {
            entry.output = Some(
                output
                    .split(|byte| *byte == 0)
                    .next()
                    .unwrap_or_default()
                    .to_vec(),
            );
        }
        if complete {
            entry.running = false;
        }
        if failed {
            entry.output =
                Some(format!("<'{}' didn't start>", String::from_utf8_lossy(command)).into_bytes());
        }
    }

    pub(super) fn format_job(
        &self,
        command: &[u8],
        target: (Option<Id>, Option<Id>, Option<Id>),
        context: &ClientContext,
        depth: usize,
    ) -> Result<Vec<u8>, String> {
        if context.no_format_jobs {
            return Ok(Vec::new());
        }
        if !self.format_jobs.borrow().enabled {
            return Err("shell-job formats require a runtime and are unsupported".into());
        }
        let mut no_jobs = context.clone();
        no_jobs.no_format_jobs = true;
        let expanded = self.expand_depth(command, target, &no_jobs, depth + 1, false)?;
        let now = self.now / 1_000_000;
        let output = {
            let mut jobs = self.format_jobs.borrow_mut();
            let key = (context.client_name.clone(), command.to_vec());
            if !jobs.entries.contains_key(&key) && jobs.entries.len() >= 4096 {
                return Err("format job cache exceeds limit".into());
            }
            let old = jobs.entries.get(&key);
            let restart = old.is_none_or(|entry| {
                entry.expanded != expanded || (!entry.running && entry.started != now)
            });
            let old_output = old.and_then(|entry| entry.output.clone());
            if restart {
                let id = jobs.next;
                jobs.next += 1;
                let old = jobs.entries.insert(
                    key.clone(),
                    Entry {
                        id,
                        expanded: expanded.clone(),
                        output: old_output,
                        started: now,
                        touched: self.now,
                        running: true,
                    },
                );
                jobs.pending.push(FormatJobRequest {
                    id,
                    client: context.client_name.clone(),
                    command: expanded,
                    cwd: context.cwd.clone(),
                    replaced: old.filter(|entry| entry.running).map(|entry| entry.id),
                });
            }
            let entry = jobs.entries.get_mut(&key).unwrap();
            entry.touched = self.now;
            if entry.output.is_none() && entry.running && now.saturating_sub(entry.started) > 1 {
                entry.output = Some(
                    format!("<'{}' not ready>", String::from_utf8_lossy(command)).into_bytes(),
                );
            }
            entry.output.clone().unwrap_or_default()
        };
        self.expand_depth(&output, target, &no_jobs, depth + 1, false)
    }
}
