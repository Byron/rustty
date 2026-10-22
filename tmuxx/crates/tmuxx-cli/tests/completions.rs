use std::{
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tx-complete-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tmuxx"));
        command
            .arg("--native-endpoint")
            .arg(self.0.join("native"))
            .env_remove("TMUX")
            .env_remove("TMUXX_ENDPOINT")
            .stdin(Stdio::null());
        command
    }

    fn completions(&self, shell: &str) -> Vec<u8> {
        success(
            self.command()
                .arg("-f")
                .arg(self.0.join("missing.conf"))
                .args(["completions", shell])
                .output()
                .unwrap(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn success(output: Output) -> Vec<u8> {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    output.stdout
}

#[test]
fn scripts_and_help_do_not_require_a_server_or_configuration() {
    let fixture = Fixture::new();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let script = String::from_utf8(fixture.completions(shell)).unwrap();
        for command in ["tmuxx", "kill-pane", "list-sessions", "completions"] {
            assert!(script.contains(command), "{shell} omitted {command}");
        }
    }
    let help = success(
        fixture
            .command()
            .args(["completions", "--help"])
            .output()
            .unwrap(),
    );
    assert!(String::from_utf8(help).unwrap().contains("fish"));
    assert!(!fixture.0.join("native").exists());

    for args in [vec!["completions"], vec!["completions", "unknown"]] {
        let output = fixture.command().args(args).output().unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("<SHELL>")
        );
    }
}

#[cfg(unix)]
mod fish {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn available() -> bool {
        match Command::new("fish").arg("--version").output() {
            Ok(output) => {
                assert!(output.status.success(), "{output:?}");
                let version = String::from_utf8(output.stdout).unwrap();
                let major: u32 = version
                    .split_whitespace()
                    .last()
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                if major < 4 {
                    eprintln!("skipping fish completion checks: fish 4.0+ is required ({version})");
                    return false;
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping fish completion checks: fish is not installed");
                false
            }
            Err(error) => panic!("cannot execute fish: {error}"),
        }
    }

    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
    }

    fn complete(fixture: &Fixture, line: &str) -> String {
        let path = fixture.0.join("tmuxx.fish");
        std::fs::write(&path, fixture.completions("fish")).unwrap();
        let output = Command::new("fish")
            .args([
                "--no-config",
                "-c",
                "source \"$TMUXX_COMPLETIONS\"; complete -C \"$TMUXX_COMPLETION_LINE\"",
            ])
            .env("TMUXX_COMPLETIONS", path)
            .env("TMUXX_COMPLETION_LINE", line)
            .env("TMUXX_COMPLETION_LOG", fixture.0.join("calls"))
            .env(
                "TMUXX_COMPLETION_ENDPOINT",
                fixture.0.join("selected owner"),
            )
            .env_remove("TMUX")
            .env_remove("TMUXX_ENDPOINT")
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        String::from_utf8(success(output)).unwrap()
    }

    fn has_candidate(output: &str, value: &str) -> bool {
        output
            .lines()
            .any(|line| line.split('\t').next() == Some(value))
    }

    #[test]
    fn completes_live_targets_and_aliases_without_starting_an_absent_server() {
        if !available() {
            return;
        }
        let fixture = Fixture::new();
        let prefix = format!(
            "{} --native-endpoint {}",
            quote(env!("CARGO_BIN_EXE_tmuxx")),
            quote(fixture.0.join("native").to_str().unwrap())
        );
        assert!(complete(&fixture, &format!("{prefix} kill-pane -t ")).is_empty());
        assert!(!fixture.0.join("native").exists());
        let output = complete(&fixture, &format!("{prefix} -f kill-pane spl"));
        assert!(has_candidate(&output, "split-window"), "{output}");
        assert!(has_candidate(&output, "splitw"), "{output}");
        let output = complete(&fixture, &format!("{prefix} splitw -"));
        assert!(has_candidate(&output, "-h"), "{output}");
        assert!(has_candidate(&output, "-t"), "{output}");

        let host =
            tmuxx_runtime::start(tmuxx_runtime::Options::new(fixture.0.join("native"))).unwrap();
        success(
            fixture
                .command()
                .args([
                    "-N",
                    "new-session",
                    "-d",
                    "-s",
                    "work fish",
                    "-n",
                    "editor's view",
                    "/bin/sh",
                    ";",
                    "split-window",
                    "-d",
                    "/bin/sh",
                    ";",
                    "set-buffer",
                    "-b",
                    "saved text",
                    "contents",
                ])
                .output()
                .unwrap(),
        );
        for command in ["attach-session", "attach"] {
            let output = complete(&fixture, &format!("{prefix} {command} -t "));
            assert!(has_candidate(&output, "work fish"), "{output}");
            assert!(has_candidate(&output, "$0"), "{output}");
        }
        for command in ["select-window -t", "display-panes -s"] {
            let output = complete(&fixture, &format!("{prefix} {command} "));
            assert!(has_candidate(&output, "work fish:0"), "{output}");
            assert!(has_candidate(&output, "@0"), "{output}");
            assert!(output.contains("editor's view"), "{output}");
        }
        let output = complete(&fixture, &format!("{prefix} -f kill-pane killp -t "));
        assert!(has_candidate(&output, "%0"), "{output}");
        assert!(has_candidate(&output, "%1"), "{output}");
        let output = complete(&fixture, &format!("{prefix} paste-buffer -b "));
        assert!(has_candidate(&output, "saved text"), "{output}");
        host.shutdown().unwrap();
    }

    #[test]
    fn queries_the_invoked_executable_with_only_connection_options() {
        if !available() {
            return;
        }
        let fixture = Fixture::new();
        let bin = fixture.0.join("quoted ' directory");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("tmuxx");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$TMUXX_COMPLETION_LOG\"\nprintf '%s\\t%s\\n' '%7' 'test pane'\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        for selectors in [
            "-L 'label with spaces' -S 'socket; echo unsafe' --native-endpoint 'C:\\quoted path\\owner' --imsg-schema legacy",
            "-L'label with spaces' -S'socket; echo unsafe' --native-endpoint='C:\\quoted path\\owner' --imsg-schema=legacy",
        ] {
            let output = complete(
                &fixture,
                &format!(
                    "{} {selectors} -f kill-pane -T features -vv killp -t ",
                    quote(executable.to_str().unwrap())
                ),
            );
            assert!(has_candidate(&output, "%7"), "{output}");
            let calls = std::fs::read(fixture.0.join("calls")).unwrap();
            let args: Vec<_> = calls
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| std::str::from_utf8(arg).unwrap())
                .collect();
            assert_eq!(
                &args[..11],
                [
                    "-L",
                    "label with spaces",
                    "-S",
                    "socket; echo unsafe",
                    "--native-endpoint",
                    "C:\\quoted path\\owner",
                    "--imsg-schema",
                    "legacy",
                    "-N",
                    "list-panes",
                    "-a",
                ]
            );
            assert_eq!(args.len(), 13, "{args:?}");
            assert_eq!(args[11], "-F");
        }

        let output = complete(
            &fixture,
            &format!(
                "{} --native-endpoint $TMUXX_COMPLETION_ENDPOINT -S ~/tmuxx-completion-owner -L (touch completion-was-executed) killp -t ",
                quote(executable.to_str().unwrap())
            ),
        );
        assert!(has_candidate(&output, "%7"), "{output}");
        assert!(!fixture.0.join("completion-was-executed").exists());
        let calls = std::fs::read_to_string(fixture.0.join("calls")).unwrap();
        let args: Vec<_> = calls.split_terminator('\0').collect();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(
            &args[..7],
            [
                "-L",
                "(touch completion-was-executed)",
                "-S",
                home.join("tmuxx-completion-owner").to_str().unwrap(),
                "--native-endpoint",
                fixture.0.join("selected owner").to_str().unwrap(),
                "-N",
            ]
        );
    }
}
