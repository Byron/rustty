//! Clipboard replies and buffers from an isolated C process, not saved goldens.
#[cfg(windows)]
use common::remote_reference;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
use std::process::Output;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tmuxx_core::{ClientContext, Effect, Engine};
mod common;

struct Reference {
    #[cfg(unix)]
    binary: PathBuf,
    #[cfg(unix)]
    directory: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
}
impl Reference {
    fn start(set: Option<&str>, get: Option<&str>, initial: bool, bytes: &[u8]) -> Self {
        #[cfg(unix)]
        let reference = {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = PathBuf::from(format!(
                "/tmp/txclip-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self {
                binary: std::env::var_os("TMUXX_C_TMUX")
                    .expect("set TMUXX_C_TMUX")
                    .into(),
                directory,
            }
        };
        #[cfg(windows)]
        let reference = Self {
            remote: remote_reference::RemoteReference::new(),
        };
        let mut config = String::new();
        for (name, value) in [("set-clipboard", set), ("get-clipboard", get)] {
            if let Some(value) = value {
                config.push_str(&format!("set-option -s {name} {value}\n"));
            }
        }
        #[cfg(unix)]
        std::fs::write(reference.directory.join("config"), &config).unwrap();
        #[cfg(windows)]
        if !config.is_empty() {
            let output = reference.remote.source(config.as_bytes());
            assert!(output.status.success(), "{output:?}");
        }
        let payload = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let script = format!(
            "import os,tty,re\nfrom pathlib import Path\ntty.setraw(0)\nopen('{}','wb').close()\nos.read(0,1)\nos.write(1,bytes.fromhex('{payload}')+b'\\x1b[c')\ndata=b''\nwhile not re.search(rb'\\x1b\\[\\?[0-9;]+c$',data): data+=os.read(0,1)\nreply=Path('{}')\ntemporary=reply.with_suffix('.tmp')\ntemporary.write_bytes(re.sub(rb'\\x1b\\[\\?[0-9;]+c$',''.encode(),data))\ntemporary.replace(reply)\nos.read(0,1)\n",
            reference.path("ready"),
            reference.path("reply")
        );
        reference.write("child.py", script.as_bytes());
        let output = reference.command(&[
            "new-session",
            "-d",
            "-x",
            "80",
            "-y",
            "24",
            "python3",
            "-u",
            &reference.path("child.py"),
        ]);
        assert!(output.status.success(), "{output:?}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !reference.exists("ready") {
            assert!(Instant::now() < deadline, "C child did not become ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        for (name, value) in [("set-clipboard", set), ("get-clipboard", get)] {
            if let Some(value) = value {
                assert!(
                    reference
                        .command(&["set-option", "-s", name, value])
                        .status
                        .success()
                );
            }
        }
        if initial {
            assert!(reference.command(&["set-buffer", "seed"]).status.success());
        }
        assert!(
            reference
                .command(&["send-keys", "-l", "q"])
                .status
                .success()
        );
        while !reference.exists("reply") {
            assert!(
                Instant::now() < deadline,
                "C did not answer DA barrier: {:?}",
                reference.command(&["capture-pane", "-p"])
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        reference
    }
    fn command(&self, args: &[&str]) -> Output {
        #[cfg(windows)]
        return self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        );
        #[cfg(unix)]
        Command::new(&self.binary)
            .arg("-f")
            .arg(self.directory.join("config"))
            .arg("-S")
            .arg(self.directory.join("s"))
            .args(args)
            .env_remove("TMUX")
            .env("TERM", "screen")
            .output()
            .unwrap()
    }
    fn path(&self, leaf: &str) -> String {
        #[cfg(unix)]
        return self.directory.join(leaf).to_str().unwrap().to_owned();
        #[cfg(windows)]
        return String::from_utf8(self.remote.path(leaf.as_bytes())).unwrap();
    }
    fn write(&self, leaf: &str, bytes: &[u8]) {
        #[cfg(unix)]
        std::fs::write(self.directory.join(leaf), bytes).unwrap();
        #[cfg(windows)]
        self.remote.put(leaf.as_bytes(), bytes);
    }
    fn exists(&self, leaf: &str) -> bool {
        #[cfg(unix)]
        return self.directory.join(leaf).exists();
        #[cfg(windows)]
        return self.remote.exists(leaf.as_bytes());
    }
    fn read(&self, leaf: &str) -> Vec<u8> {
        #[cfg(unix)]
        return std::fs::read(self.directory.join(leaf)).unwrap();
        #[cfg(windows)]
        return self.remote.get(leaf.as_bytes());
    }
}
#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        self.command(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn compare(set: Option<&str>, get: Option<&str>, initial: bool, bytes: &[u8]) {
    let c = Reference::start(set, get, initial, bytes);
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    let mut command = |args: &[&str]| {
        engine
            .execute(
                &tmuxx_command::Command {
                    blocks: Default::default(),
                    args: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
                },
                &mut context,
            )
            .unwrap();
    };
    command(&["new-session", "-d"]);
    for (name, value) in [("set-clipboard", set), ("get-clipboard", get)] {
        if let Some(value) = value {
            command(&["set-option", "-s", name, value]);
        }
    }
    if initial {
        engine.store_buffer(None, b"seed".to_vec()).unwrap();
    }
    let effects = engine.feed(0, bytes);
    let reply = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Write { data, .. } => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        reply,
        c.read("reply"),
        "set={set:?} get={get:?} input={bytes:?}"
    );
    let names = c.command(&["list-buffers", "-F", "#{buffer_name}"]);
    assert!(names.status.success(), "{names:?}");
    let mut buffers = std::collections::BTreeMap::new();
    for name in String::from_utf8(names.stdout).unwrap().lines() {
        let value = c.command(&["save-buffer", "-b", name, "-"]);
        assert!(value.status.success(), "{value:?}");
        buffers.insert(name.to_owned(), value.stdout);
    }
    assert_eq!(
        engine.buffers, buffers,
        "set={set:?} get={get:?} input={bytes:?}"
    );
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn clipboard_policies_selectors_and_base64_match_c() {
    let query = b"\x1b]52;c;bmV3\x07\x1b]52;c;?\x07";
    compare(None, None, true, query);
    for set in ["off", "external", "on"] {
        for get in ["off", "buffer", "request", "both"] {
            compare(Some(set), Some(get), true, query);
        }
    }
    compare(Some("on"), Some("buffer"), false, b"\x1b]52;c;?\x07");
    for selector in ["", "cp", "pc", "x7cc", "q", "invalid"] {
        compare(
            Some("on"),
            Some("buffer"),
            true,
            format!("\x1b]52;{selector};?\x07\x1b]52;{selector};?\x1b\\").as_bytes(),
        );
    }
    for encoded in [
        "", "YQ==", "YQ", "YR==", "YWI=", "YWJ=", "YWJj", "!", "Y Q = =", " ", "====",
    ] {
        compare(
            Some("on"),
            Some("buffer"),
            false,
            format!("\x1b]52;cp;{encoded}\x07").as_bytes(),
        );
    }
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX"]
fn default_buffer_commands_skip_named_buffers_like_c() {
    let mut pair = common::Pair::new();
    for args in [
        vec!["set-buffer", "-b", "named", "private"],
        vec!["show-buffer"],
        vec!["save-buffer", "-"],
        vec!["paste-buffer"],
        vec!["delete-buffer"],
        vec!["set-buffer", "-n", "rename"],
        vec!["set-buffer", "old"],
        vec!["set-buffer", "new"],
        vec!["set-buffer", "-b", "latest-named", "named"],
        vec!["show-buffer"],
        vec!["save-buffer", "-"],
        vec!["paste-buffer", "-d"],
        vec!["show-buffer"],
        vec!["set-buffer", "-n", "renamed"],
        vec!["show-buffer"],
        vec!["paste-buffer"],
        vec!["delete-buffer"],
        vec!["show-buffer", "-b", "renamed"],
        vec!["delete-buffer", "-b", "missing"],
        vec!["set-buffer", "-b", "buffer2", "user"],
        vec!["set-buffer", "auto"],
        vec!["list-buffers", "-F", "#{buffer_name}:#{buffer_size}"],
    ] {
        pair.run(&args);
    }
}

#[test]
#[ignore = "requires C tmux and socket access"]
fn buffer_append_empty_rename_and_clipboard_flags_match_c() {
    let mut pair = common::Pair::new();
    for args in [
        vec!["set-buffer"],
        vec!["set-buffer", ""],
        vec!["set-buffer", "-b", "named", "abc"],
        vec!["set-buffer", "-a", "-b", "named", "DEF"],
        vec!["show-buffer", "-b", "named"],
        vec!["set-buffer", "-a", "-b", "missing", "new"],
        vec!["show-buffer", "-b", "missing"],
        vec!["set-buffer", "-aw", "-t", "missing-client", "auto"],
        vec!["show-buffer"],
        vec!["set-buffer", "-a", "appended"],
        vec!["show-buffer"],
        vec!["set-buffer", "-b", "named", ""],
        vec!["show-buffer", "-b", "named"],
        vec!["set-buffer", "-b", "", "data"],
        vec!["set-buffer", "-b", "named", "-n", "renamed", "ignored"],
        vec!["show-buffer", "-b", "renamed"],
        vec!["set-buffer", "-b", "renamed", "-n", "renamed"],
    ] {
        pair.run(&args);
    }
}

#[test]
#[ignore = "requires C tmux and socket access"]
fn named_capture_buffers_preserve_bytes_ownership_and_print_precedence() {
    let mut pair = common::Pair::new();
    pair.run(&["new-window", "-dI", "-t", "main:1", "-n", "empty"]);
    pair.feed("%1", 1, b"alpha\r\n\x1b[31mred\x1b[0m\r\n", 1);
    for args in [
        vec!["set-buffer", "-b", "untouched", "user"],
        vec![
            "capture-pane",
            "-t",
            "%1",
            "-b",
            "capture",
            "-S",
            "0",
            "-E",
            "1",
        ],
        vec!["show-buffer", "-b", "capture"],
        vec!["show-buffer", "-b", "untouched"],
        vec![
            "capture-pane",
            "-p",
            "-t",
            "%1",
            "-b",
            "capture",
            "-e",
            "-S",
            "1",
            "-E",
            "1",
        ],
        vec!["show-buffer", "-b", "capture"],
        vec!["capture-pane", "-t", "%1", "-b", "", "-S", "0", "-E", "1"],
        vec!["capture-pane", "-t", "%1", "-S", "1", "-E", "1"],
        vec!["show-buffer"],
        vec!["list-buffers", "-F", "#{buffer_name}:#{buffer_size}"],
    ] {
        pair.run(&args);
    }
}
