#![cfg(any(unix, windows))]
#[cfg(windows)]
#[path = "identity/windows.rs"]
mod windows;

#[cfg(unix)]
mod unix {
    use std::{
        io::{Read, Write},
        os::{fd::AsFd, unix::net::UnixStream},
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    use tmuxx_protocol::{Decoder, Event};
    use tmuxx_unix_ipc::{Connection, Identity, Schema, identify, msg, pack_argv};

    struct Control {
        _connection: Connection,
        input: UnixStream,
        output: UnixStream,
        decoder: Decoder,
    }
    impl Control {
        fn connect(path: &Path, pid: i32, session: &str) -> Self {
            let mut connection = Connection::new(
                UnixStream::connect(path).unwrap(),
                Schema::Modern { version: 8 },
            )
            .unwrap();
            let (input, writer) = UnixStream::pair().unwrap();
            let (reader, output) = UnixStream::pair().unwrap();
            reader
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut identity = Identity::current(true).unwrap();
            identity.pid = pid;
            identity.features = (1 << 4) | (1 << 18);
            identity.term = b"tmuxx-identity-test".to_vec();
            identity.cwd = b"/tmp".to_vec();
            identity.environment = vec![b"USER=forged-user-must-not-be-used".to_vec()];
            identify(
                &mut connection,
                &identity,
                Some(input.as_fd()),
                Some(output.as_fd()),
            )
            .unwrap();
            connection
                .send(
                    msg::COMMAND,
                    &pack_argv(&[
                        b"attach-session".to_vec(),
                        b"-t".to_vec(),
                        session.as_bytes().to_vec(),
                    ])
                    .unwrap(),
                    None,
                )
                .unwrap();
            let mut control = Self {
                _connection: connection,
                input: writer,
                output: reader,
                decoder: Decoder::default(),
            };
            assert_eq!(control.reply(), Vec::<u8>::new());
            control
        }
        fn reply(&mut self) -> Vec<u8> {
            let mut result = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let n = self.output.read(&mut buffer).unwrap();
                assert_ne!(n, 0);
                let mut ended = false;
                self.decoder
                    .feed_into(&buffer[..n], |event| match event {
                        Event::CommandOutput(line) => {
                            result.extend(line);
                            result.push(b'\n');
                        }
                        Event::End { failed, .. } => {
                            assert!(!failed, "{}", String::from_utf8_lossy(&result));
                            ended = true;
                        }
                        _ => {}
                    })
                    .unwrap();
                if ended {
                    return result;
                }
            }
        }
        fn run(&mut self, command: &str) -> Vec<u8> {
            self.input.write_all(command.as_bytes()).unwrap();
            self.input.write_all(b"\n").unwrap();
            self.reply()
        }
    }

    pub(super) fn authenticated_client_identity_and_graph_lists_match_c() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let prefix = format!(
            "/tmp/tx-id-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let native = PathBuf::from(format!("{prefix}.n"));
        let rust = PathBuf::from(format!("{prefix}.r"));
        let c = PathBuf::from(format!("{prefix}.c"));
        let binary = std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX");
        let mut options = tmuxx_runtime::Options::new(native);
        options.tmux_endpoint = Some(rust.clone());
        let host = tmuxx_runtime::start(options).unwrap();
        struct Cleanup {
            binary: std::ffi::OsString,
            c: PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new(&self.binary)
                    .arg("-S")
                    .arg(&self.c)
                    .arg("kill-server")
                    .output();
            }
        }
        let _cleanup = Cleanup {
            binary: binary.clone(),
            c: c.clone(),
        };
        for socket in [&c, &rust] {
            for args in [
                vec![
                    "new-session",
                    "-d",
                    "-s",
                    "a",
                    "-n",
                    "one",
                    "-c",
                    "/tmp",
                    "sleep 120",
                ],
                vec!["new-session", "-d", "-s", "b", "-t", "a", "-c", "/"],
            ] {
                let output = Command::new(&binary)
                    .args(["-f", "/dev/null", "-S"])
                    .arg(socket)
                    .args(args)
                    .env_remove("TMUX")
                    .env("SHELL", "/bin/sh")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        let scope: serde_json::Value =
            serde_json::from_str(include_str!("../../../conformance/scope.json")).unwrap();
        let mut fields = scope["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|group| group["id"] == "format-absent-context")
            .unwrap()["formats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        fields.extend(
            [
                "client_control_mode",
                "client_cell_width",
                "client_cell_height",
                "client_width",
                "client_height",
                "client_flags",
                "client_name",
                "client_session",
                "client_readonly",
                "client_colours",
                "client_theme",
                "client_created",
                "client_activity",
                "client_pid",
                "client_uid",
                "client_user",
                "client_termname",
                "client_termtype",
                "client_termfeatures",
                "client_tty",
                "client_utf8",
                "client_last_session",
                "client_written",
                "client_discarded",
                "window_bigger",
            ]
            .map(String::from),
        );
        let absent_format = fields
            .iter()
            .map(|field| format!("{field}=#{{{field}}}"))
            .collect::<Vec<_>>()
            .join("|");
        for args in [
            vec!["display-message", "-p", "-t", "a", &absent_format],
            vec!["list-commands", "-F", &absent_format, "new-session"],
        ] {
            let run = |socket: &Path| {
                let result = Command::new(&binary)
                    .args(["-f", "/dev/null", "-S"])
                    .arg(socket)
                    .args(&args)
                    .env_remove("TMUX")
                    .output()
                    .unwrap();
                assert!(result.status.success(), "{result:?}");
                result.stdout
            };
            assert_eq!(
                String::from_utf8_lossy(&run(&rust)),
                String::from_utf8_lossy(&run(&c)),
                "absent contexts {args:?}"
            );
        }
        let (mut rc, mut cc) = (
            Control::connect(&rust, 45002, "a"),
            Control::connect(&c, 45002, "a"),
        );
        let (mut rsecond, mut csecond) = (
            Control::connect(&rust, 45001, "b"),
            Control::connect(&c, 45001, "b"),
        );
        let identity = "#{client_name}|#{client_pid}|#{client_uid}|#{client_user}|#{client_termname}|#{client_termtype}|#{client_termfeatures}|#{client_tty}|#{client_utf8}|#{client_cwd}|#{client_session}|#{client_last_session}";
        let session = "#{session_name}|#{session_path}|#{session_active}|#{session_attached}|#{session_many_attached}|#{session_attached_list}|#{session_group_attached}|#{session_group_attached_list}|#{session_group_many_attached}";
        let window = "#{window_active_clients}|#{window_active_clients_list}|#{window_active_sessions}|#{window_active_sessions_list}|#{window_linked_sessions_list}";
        let tty = fields
            .iter()
            .filter(|field| !matches!(field.as_str(), "client_activity" | "client_created"))
            .map(|field| format!("{field}=#{{{field}}}"))
            .collect::<Vec<_>>()
            .join("|");
        let kinds =
            "#{session_format}#{window_format}#{pane_format}:#{session_id}:#{window_id}:#{pane_id}";
        let nested = format!("{kinds}|#{{L:{kinds};}}");
        for command in [
            format!("display -p '{identity}'"),
            format!("list-sessions -F '{session}'"),
            format!("list-windows -aF '{window}'"),
            format!("display -p '#{{L:{identity};}}'"),
            "display -p '#{host}|#{host_short}|#{uid}|#{user}'".into(),
            format!("display -p '{tty}'"),
            format!("display -p -t b -c client-45002: '{tty}'"),
            format!("list-sessions -F '{tty}'"),
            format!("list-windows -F '{tty}'"),
            format!("list-panes -F '{tty}'"),
            format!("list-commands -F '{absent_format}' new-session"),
            format!("display -p '{nested}'"),
            format!("list-sessions -F '{nested}'"),
            format!("list-windows -F '{nested}'"),
            format!("list-panes -F '{nested}'"),
            format!("list-commands -F '{nested}' new-session"),
        ] {
            assert_eq!(rc.run(&command), cc.run(&command), "{command}");
        }
        assert_eq!(
            rc.run("display -p '#{socket_path}'"),
            format!("{}\n", rust.display()).as_bytes()
        );
        assert_eq!(
            cc.run("display -p '#{socket_path}'"),
            format!("{}\n", c.display()).as_bytes()
        );
        assert_eq!(
            rc.run("display -p '#{pid}'"),
            format!("{}\n", std::process::id()).as_bytes()
        );
        assert!(
            String::from_utf8(cc.run("display -p '#{pid}'"))
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap()
                > 0
        );
        for order in [
            "",
            " -r",
            " -O order",
            " -O order -r",
            " -O name",
            " -O name -r",
            " -O creation",
            " -O activity",
            " -O index",
            " -O modifier",
            " -O z",
            " -O size",
            " -O title -t b",
            " -f '#{==:client_session,b}'",
        ] {
            let command = format!(
                "list-clients{order} -F '#{{line}}:{identity}|#{{session_format}},#{{window_format}},#{{pane_format}}'"
            );
            assert_eq!(rc.run(&command), cc.run(&command), "{command}");
        }
        assert_eq!(
            rsecond.run("switch-client -t a"),
            csecond.run("switch-client -t a")
        );
        assert_eq!(
            rsecond.run(&format!("display -p '{identity}'")),
            csecond.run(&format!("display -p '{identity}'"))
        );
        assert_eq!(
            rc.run(&format!("list-sessions -F '{session}'")),
            cc.run(&format!("list-sessions -F '{session}'"))
        );
        assert_eq!(rc.run("kill-session -t b"), cc.run("kill-session -t b"));
        assert_eq!(
            rsecond.run(&format!("display -p '{identity}'")),
            csecond.run(&format!("display -p '{identity}'"))
        );
        let counters = "display -p '#{client_written}|#{client_discarded}'";
        let output_pane = "split-window -d -P -F '#{pane_id}' 'printf control-output; sleep 120'";
        let pane = rc.run(output_pane);
        assert_eq!(pane, cc.run(output_pane));
        let capture = format!(
            "capture-pane -p -t {}",
            String::from_utf8(pane).unwrap().trim()
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let ready = [&mut rc, &mut cc]
                .map(|client| client.run(&capture).starts_with(b"control-output"));
            if ready.iter().all(|ready| *ready) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "PTY output did not arrive"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        for client in [&mut rc, &mut cc] {
            assert_eq!(client.run(counters), b"0|0\n");
        }
        drop((rc, cc, rsecond, csecond));
        host.shutdown().unwrap();
    }
}

#[test]
#[ignore = "requires an explicitly selected C reference and native process access"]
fn authenticated_client_identity_and_graph_lists_match_c() {
    #[cfg(unix)]
    unix::authenticated_client_identity_and_graph_lists_match_c();
    #[cfg(windows)]
    windows::identity_matrix();
}
