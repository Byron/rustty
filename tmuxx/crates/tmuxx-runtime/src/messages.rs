use super::*;
use tmuxx_command::{Args, Command};

const MAX_MESSAGE_BYTES: usize = 4096;

fn bounded_text(text: &[u8]) -> Vec<u8> {
    let mut data = tmuxx_command::display_argument(&text[..text.len().min(MAX_MESSAGE_BYTES)]);
    if text.len() > MAX_MESSAGE_BYTES || data.len() > MAX_MESSAGE_BYTES {
        data.truncate(MAX_MESSAGE_BYTES);
        data.extend(b" [truncated]");
    }
    data
}

impl Host {
    pub(super) fn trim_messages(&mut self) {
        let limit = self
            .engine
            .server_options
            .get("message-limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1000)
            .min(tmuxx_core::MAX_DIAGNOSTIC_MESSAGES);
        while self.messages.len() > limit {
            self.messages.pop_front();
        }
    }

    pub(super) fn log_message(&mut self, kind: &[u8], text: &[u8]) {
        let mut data = kind.to_vec();
        data.extend(b": ");
        data.extend(bounded_text(text));
        self.messages.push_back((self.engine.now / 1_000_000, data));
        self.trim_messages();
    }

    pub(super) fn log_command(&mut self, command: &Command) {
        if command.args.first().is_some_and(|name| {
            matches!(name.as_slice(), b"show-messages" | b"showmsgs") || name.starts_with(b"tmuxx-")
        }) {
            return;
        }
        self.log_message(
            b"command",
            &tmuxx_command::serialize(std::slice::from_ref(command)),
        );
    }

    pub(super) fn show_messages(&mut self, command: &Command) -> Result<Outcome, String> {
        let args = Args::parse(&command.args[1..], "JTt:")?;
        args.no_rest()?;
        self.trim_messages();
        let mut out = Outcome::default();
        if args.has(b'J') {
            let jobs = self.scheduler.job_summaries().chain(
                self.format_jobs
                    .iter()
                    .map(|(id, job)| (*id, job.command(), "format")),
            );
            for (number, (id, command, kind)) in jobs.enumerate() {
                out.output.extend(format!("Job {number}: ").as_bytes());
                out.output.extend(bounded_text(command));
                out.output
                    .extend(format!(" [id={id}, kind={kind}]\n").as_bytes());
            }
        } else if !args.has(b'T') {
            for (time, text) in self.messages.iter().rev() {
                out.output.extend(format!("tmuxx {time}: ").as_bytes());
                out.output.extend(text);
                out.output.push(b'\n');
            }
        }
        // Native/control clients have no physical tty terminfo instances for -T.
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_bounds_count_escaped_bytes_and_mark_truncation() {
        for text in [
            vec![b'x'; MAX_MESSAGE_BYTES + 1],
            vec![1; MAX_MESSAGE_BYTES],
        ] {
            let result = bounded_text(&text);
            assert_eq!(result.len(), MAX_MESSAGE_BYTES + b" [truncated]".len());
            assert!(result.ends_with(b" [truncated]"));
            assert!(!result.iter().any(u8::is_ascii_control));
        }
        assert_eq!(bounded_text(b"short\ntext"), b"short\\ntext");
        assert_eq!(
            bounded_text(&vec![b'x'; MAX_MESSAGE_BYTES]).len(),
            MAX_MESSAGE_BYTES
        );
    }
}
