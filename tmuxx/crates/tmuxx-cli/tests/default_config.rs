#![cfg(unix)]

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Pending(Option<Child>);

impl Pending {
    fn finish(mut self) -> io::Result<Output> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.0.as_mut().unwrap().try_wait()?.is_some() {
                return self.0.take().unwrap().wait_with_output();
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "tmuxx client did not finish",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Server {
    directory: PathBuf,
    xdg: Option<OsString>,
}

impl Server {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(format!(
            "/tmp/tx-default-cfg-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir(directory.join("home")).unwrap();
        Self {
            directory,
            xdg: None,
        }
    }

    fn home_config(&self) -> PathBuf {
        self.directory.join("home/.config/tmuxx/tmux.conf")
    }

    fn xdg_config(&self) -> PathBuf {
        self.directory.join("xdg/tmuxx/tmux.conf")
    }

    fn spawn(&self, args: &[&str]) -> io::Result<Pending> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tmuxx"));
        command
            .arg("-S")
            .arg(self.directory.join("socket"))
            .args(args)
            .current_dir(&self.directory)
            .env("HOME", self.directory.join("home"))
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUXX_ENDPOINT")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(xdg) = &self.xdg {
            command.env("XDG_CONFIG_HOME", xdg);
        }
        command.spawn().map(|child| Pending(Some(child)))
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.spawn(args).unwrap().finish().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
        output
    }

    fn value(&self, format: &str) -> Vec<u8> {
        self.run(&["display-message", "-p", format]).stdout
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(pending) = self.spawn(&["-N", "-f", "/dev/null", "kill-server"]) {
            let _ = pending.finish();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn write_config(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn wait_for_contents(path: &Path, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let actual = std::fs::read(path).ok();
        if actual.as_deref() == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "configured pane command did not write {expected:?}: {actual:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn home_default_config_controls_the_first_pane_and_loads_once() {
    let server = Server::new();
    let marker = server.directory.join("panes");
    write_config(
        &server.home_config(),
        &format!(
            "set -ga @loads x\nset -g default-shell /bin/sh\n\
             set -g default-command 'printf x >> {}; exec /bin/sh'\n",
            marker.display()
        ),
    );

    server.run(&["new-session", "-d", "-s", "first"]);
    wait_for_contents(&marker, b"x");
    assert_eq!(server.value("#{@loads}"), b"x\n");

    server.run(&["new-window", "-d", "-t", "first"]);
    wait_for_contents(&marker, b"xx");
    assert_eq!(server.value("#{@loads}"), b"x\n");
}

#[test]
fn xdg_config_precedes_home_and_invalid_xdg_values_fall_back() {
    for xdg in [None, Some(""), Some("relative"), Some("absolute")] {
        let mut server = Server::new();
        write_config(&server.home_config(), "set -g @source home\n");
        write_config(&server.xdg_config(), "set -g @source xdg\n");
        write_config(
            &server.directory.join("relative/tmuxx/tmux.conf"),
            "set -g @source relative\n",
        );
        server.xdg = xdg.map(|value| {
            if value == "absolute" {
                server.directory.join("xdg").into_os_string()
            } else {
                value.into()
            }
        });

        server.run(&["new-session", "-d", "-s", "first", "/bin/sh"]);
        let expected = if xdg == Some("absolute") {
            b"xdg\n".as_slice()
        } else {
            b"home\n".as_slice()
        };
        assert_eq!(server.value("#{@source}"), expected, "XDG={xdg:?}");
    }
}

#[test]
fn explicit_configs_preserve_order_and_replace_default_discovery() {
    for empty in [false, true] {
        let mut server = Server::new();
        write_config(
            &server.home_config(),
            "set -g @source home\nset -g @default_loaded 1\n",
        );
        write_config(
            &server.xdg_config(),
            "set -g @source xdg\nset -g @default_loaded 1\n",
        );
        server.xdg = Some(server.directory.join("xdg").into_os_string());
        let explicit = server.directory.join("explicit.conf");
        write_config(&explicit, "set -g @source explicit\n");
        let second = server.directory.join("second.conf");
        write_config(&second, "set -ga @source second\n");
        let config = if empty {
            "/dev/null"
        } else {
            explicit.to_str().unwrap()
        };

        let mut args = vec!["-f", config];
        if !empty {
            args.extend(["-f", second.to_str().unwrap()]);
        }
        args.extend(["new-session", "-d", "-s", "first", "/bin/sh"]);
        server.run(&args);
        assert_eq!(server.value("#{@default_loaded}"), b"\n");
        assert_eq!(
            server.value("#{@source}"),
            if empty {
                b"\n".as_slice()
            } else {
                b"explicitsecond\n".as_slice()
            }
        );
    }
}

#[test]
fn a_missing_default_config_is_harmless_without_home_fallback_from_xdg() {
    for xdg in [false, true] {
        let mut server = Server::new();
        if xdg {
            write_config(&server.home_config(), "set -g @source home\n");
            server.xdg = Some(server.directory.join("missing-xdg").into_os_string());
        }

        server.run(&["new-session", "-d", "-s", "first", "/bin/sh"]);
        assert_eq!(server.value("#{session_name} #{@source}"), b"first \n");
    }
}
