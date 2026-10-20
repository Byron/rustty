#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

struct Server(PathBuf);

impl Server {
    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rustty"))
            .arg("tmux")
            .args(["-f", "/dev/null"])
            .args(args)
            .env("TMUX_TMPDIR", &self.0)
            .env("TMUX", "/an/unrelated/tmux/socket,123,0")
            .env_remove("TMUXX_ENDPOINT")
            .output()
            .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.command(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn tmux_subcommand_starts_native_daemon_and_reattaches_without_gui() {
    let directory = PathBuf::from(format!("/tmp/rustty-tmux-cli-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let server = Server(directory);
    success(server.command(&["new-session", "-d", "-s", "proof", "sleep", "30"]));
    assert_eq!(
        success(server.command(&["list-sessions", "-F", "#{session_name}"])),
        b"proof\n"
    );
    let endpoint = std::fs::read_dir(&server.0)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(endpoint.join("default.native").exists());
    assert!(!endpoint.join("default").exists());
    let daemon_pid = String::from_utf8(success(server.command(&[
        "display-message",
        "-p",
        "#{pid}",
    ])))
    .unwrap()
    .trim()
    .parse::<libc::pid_t>()
    .unwrap();
    assert!(daemon_pid > 0);
    assert_eq!(unsafe { libc::getsid(daemon_pid) }, daemon_pid);
    assert_eq!(unsafe { libc::getpgid(daemon_pid) }, daemon_pid);
    let pid = success(server.command(&["list-panes", "-t", "proof:", "-F", "#{pane_pid}"]));
    assert_eq!(
        success(server.command(&["list-panes", "-t", "proof:", "-F", "#{pane_pid}"])),
        pid
    );
}
