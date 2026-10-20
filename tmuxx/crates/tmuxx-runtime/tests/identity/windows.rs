#[path = "../../../../conformance/remote_reference.rs"]
#[allow(dead_code)]
mod remote_reference;
use remote_reference::{RemoteControl, RemoteReference};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
use tmuxx_protocol::{
    Decoder, Event,
    native::{ClientMessage, ControlRequest, Request, ServerMessage},
};
use tmuxx_runtime::{ClientContext, LocalClient, Options, ServerHandle};

struct Fixture {
    host: Option<ServerHandle>,
    directory: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.host.take());
        let result = std::fs::remove_dir_all(&self.directory);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}
fn command(client: &LocalClient, args: &[&str]) -> Vec<u8> {
    client
        .send(Request {
            request_id: 1,
            argv: args.iter().map(|a| a.as_bytes().to_vec()).collect(),
            stdin: None,
            subscribe: false,
            control: false,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{args:?}");
            return reply.output;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn ccommand(reference: &RemoteReference, args: &[&str]) -> Vec<u8> {
    let result = reference.output(&args.iter().map(|v| v.as_bytes()).collect::<Vec<_>>(), &[]);
    assert!(
        result.status.success() && result.stderr.is_empty(),
        "{args:?}: {result:?}"
    );
    result.stdout
}
struct Native {
    child: Child,
    input: ChildStdin,
    output: Receiver<Vec<u8>>,
    request: u64,
    decoder: Decoder,
}
impl Drop for Native {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Native {
    fn new(endpoint: &Path, directory: &Path, session: &str) -> Self {
        use std::os::windows::process::CommandExt;
        let mut child = Command::new(std::env::var_os("TMUXX_REMOTE_PYTHON").unwrap())
            .args(["-u", "-c", include_str!("windows_peer.py")])
            .arg(endpoint)
            .current_dir(directory)
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                let value: Vec<u8> = serde_json::from_str(&line).unwrap();
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            input,
            output: rx,
            request: 0,
            decoder: Decoder::default(),
        };
        assert!(
            client
                .run(&format!("attach-session -t {session}"))
                .is_empty()
        );
        client
    }
    fn run(&mut self, source: &str) -> Vec<u8> {
        self.request += 1;
        let request = ClientMessage::Control(ControlRequest {
            request_id: self.request,
            argv: vec![],
            source: Some(source.as_bytes().to_vec()),
            subscribe: self.request == 1,
        });
        serde_json::to_writer(&mut self.input, &request).unwrap();
        self.input.write_all(b"\n").unwrap();
        self.input.flush().unwrap();
        let data = self
            .output
            .recv_timeout(Duration::from_secs(10))
            .expect("native identity peer timeout");
        decoded(&mut self.decoder, &data).expect("native batch included a complete command guard")
    }
}
struct Control {
    remote: RemoteControl,
    decoder: Decoder,
}
impl Control {
    fn new(reference: &RemoteReference, session: &str) -> Self {
        let remote = reference.control_open_args(&[
            b"-T",
            b"clipboard,RGB",
            b"attach-session",
            b"-t",
            session.as_bytes(),
        ]);
        let mut client = Self {
            remote,
            decoder: Decoder::default(),
        };
        assert!(client.reply().is_empty());
        client
    }
    fn reply(&mut self) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut result = Vec::new();
        loop {
            let output = self.remote.read();
            assert!(
                output.status.success() && !output.stdout.is_empty(),
                "{output:?}"
            );
            let mut ended = false;
            for event in self.decoder.feed(&output.stdout).unwrap() {
                match event {
                    Event::CommandOutput(line) => {
                        result.extend(line);
                        result.push(b'\n');
                    }
                    Event::End { failed, .. } => {
                        assert!(!failed, "{:?}", String::from_utf8_lossy(&result));
                        ended = true;
                    }
                    _ => {}
                }
            }
            if ended {
                return result;
            }
            assert!(Instant::now() < deadline);
        }
    }
    fn run(&mut self, source: &str) -> Vec<u8> {
        let result = self.remote.write(format!("{source}\n").as_bytes());
        assert!(result.status.success());
        self.reply()
    }
}
fn decoded(decoder: &mut Decoder, data: &[u8]) -> Option<Vec<u8>> {
    let mut result = Vec::new();
    let mut ended = false;
    for event in decoder.feed(data).unwrap() {
        match event {
            Event::CommandOutput(line) => {
                result.extend(line);
                result.push(b'\n');
            }
            Event::End { failed, .. } => {
                assert!(!failed, "{:?}", String::from_utf8_lossy(&result));
                ended = true;
            }
            _ => {}
        }
    }
    ended.then_some(result)
}
fn quote(value: &str) -> String {
    String::from_utf8(tmuxx_command::quote_argument(value.as_bytes())).unwrap()
}
fn text(value: Vec<u8>) -> String {
    String::from_utf8(value).unwrap()
}
fn windows_name(function: unsafe extern "system" fn(*mut u16, *mut u32) -> i32) -> String {
    let mut data = vec![0u16; 32768];
    let mut length = data.len() as u32;
    assert_ne!(unsafe { function(data.as_mut_ptr(), &mut length) }, 0);
    let end = data.iter().position(|value| *value == 0).unwrap();
    String::from_utf16(&data[..end]).unwrap()
}
struct Mapping {
    pairs: Vec<(String, String)>,
}
impl Mapping {
    fn canonical(&self, bytes: &[u8]) -> String {
        let text = std::str::from_utf8(bytes).unwrap();
        // Only complete field/list tokens map. Never replace arbitrary digits,
        // substrings, terminal templates, delimiters or ordering.
        let mut output = String::new();
        let mut word = String::new();
        let flush = |word: &mut String, output: &mut String| {
            output.push_str(
                self.pairs
                    .iter()
                    .find(|(actual, _)| actual == word)
                    .map_or(word.as_str(), |(_, logical)| logical),
            );
            word.clear();
        };
        for ch in text.chars() {
            if matches!(ch, '|' | '\n' | '=' | ';' | ',' | ':') {
                flush(&mut word, &mut output);
                output.push(ch);
            } else {
                word.push(ch);
            }
        }
        flush(&mut word, &mut output);
        output
    }
}

pub(super) fn identity_matrix() {
    let directory = std::env::temp_dir().join(format!("tx-identity-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let endpoint =
        tmuxx_runtime::windows::endpoint(directory.file_name().unwrap().to_str().unwrap()).unwrap();
    let host = tmuxx_runtime::start(Options::new(&endpoint)).unwrap();
    let fixture = Fixture {
        host: Some(host),
        directory: directory.clone(),
    };
    let reference = RemoteReference::new_with_environment(&[
        (b"TERM", Some(b"tmuxx-identity-test")),
        (b"USER", Some(b"forged-user-must-not-be-used")),
    ]);
    let remote_dir = text(reference.directory());
    let local = directory.to_str().unwrap();
    let admin = fixture
        .host
        .as_ref()
        .unwrap()
        .connect_with_context(ClientContext {
            cwd: Some(local.into()),
            ..Default::default()
        })
        .unwrap();
    let python = std::env::var("TMUXX_REMOTE_PYTHON").unwrap();
    assert!(
        command(
            &admin,
            &[
                "new-session",
                "-d",
                "-s",
                "a",
                "-n",
                "one",
                "-c",
                local,
                &python,
                "-c",
                "import time; time.sleep(120)"
            ]
        )
        .is_empty()
    );
    assert!(
        ccommand(
            &reference,
            &[
                "new-session",
                "-d",
                "-s",
                "a",
                "-n",
                "one",
                "-c",
                &remote_dir,
                "sleep 120"
            ]
        )
        .is_empty()
    );
    let second_dir = directory.join("second");
    std::fs::create_dir(&second_dir).unwrap();
    let second_local = second_dir.to_str().unwrap();
    // Preserve distinct grouped-session paths using a second real private path.
    ccommand(
        &reference,
        &["run-shell", &format!("mkdir {}/second", quote(&remote_dir))],
    );
    command(
        &admin,
        &[
            "new-session",
            "-d",
            "-s",
            "b",
            "-t",
            "a",
            "-c",
            second_local,
        ],
    );
    ccommand(
        &reference,
        &[
            "new-session",
            "-d",
            "-s",
            "b",
            "-t",
            "a",
            "-c",
            &format!("{remote_dir}/second"),
        ],
    );
    let scope: serde_json::Value =
        serde_json::from_str(include_str!("../../../../conformance/scope.json")).unwrap();
    let mut fields = scope["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "format-absent-context")
        .unwrap()["formats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_owned())
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
        .map(|f| format!("{f}=#{{{f}}}"))
        .collect::<Vec<_>>()
        .join("|");
    for args in [
        vec!["display-message", "-p", "-t", "a", &absent_format],
        vec!["list-commands", "-F", &absent_format, "new-session"],
    ] {
        assert_eq!(
            command(&admin, &args),
            ccommand(&reference, &args),
            "absent {args:?}"
        );
    }
    drop(admin);
    let (mut rc, mut cc) = (
        Native::new(&endpoint, &directory, "a"),
        Control::new(&reference, "a"),
    );
    let (mut rsecond, mut csecond) = (
        Native::new(&endpoint, &directory, "b"),
        Control::new(&reference, "b"),
    );
    let mut native = Mapping { pairs: vec![] };
    let mut c = Mapping { pairs: vec![] };
    let mut first_name = String::new();
    let mut cfirst_name = String::new();
    let mut native_names = Vec::new();
    let mut c_names = Vec::new();
    for (index, (r, cpeer)) in [(&mut rc, &mut cc), (&mut rsecond, &mut csecond)]
        .into_iter()
        .enumerate()
    {
        let rvalue = text(r.run("display -p '#{client_name}|#{client_pid}|#{client_uid}|#{client_user}|#{client_termname}|#{client_termfeatures}|#{client_utf8}|#{client_cwd}'"));
        let cvalue = text(cpeer.run("display -p '#{client_name}|#{client_pid}|#{client_uid}|#{client_user}|#{client_termname}|#{client_termfeatures}|#{client_utf8}|#{client_cwd}'"));
        let rv = rvalue.trim_end_matches('\n').split('|').collect::<Vec<_>>();
        let cv = cvalue.trim_end_matches('\n').split('|').collect::<Vec<_>>();
        assert_eq!(rv.len(), 8);
        assert_eq!(cv.len(), 8);
        assert_eq!(rv[1], r.child.id().to_string());
        assert_eq!(cv[1], cpeer.remote.pid.to_string());
        assert_eq!(cv[0].as_bytes(), cpeer.remote.name);
        native_names.push(rv[0].to_owned());
        c_names.push(cv[0].to_owned());
        assert_eq!(rv[2], tmuxx_runtime::windows::account_sid().unwrap());
        assert_eq!(&rv[4..7], &["tmuxx-identity-test", "clipboard,RGB", "1"]);
        assert_eq!(&cv[4..7], &rv[4..7]);
        // The pinned C registry has no client_cwd format callback. Preserve
        // that original absent-format vector; cwd still enters both clients.
        assert_eq!(rv[7], "");
        assert_eq!(cv[7], "");
        assert_ne!(rv[3], "forged-user-must-not-be-used");
        assert_eq!(
            rv[3],
            windows_name(windows_sys::Win32::System::WindowsProgramming::GetUserNameW)
        );
        assert_ne!(cv[3], "forged-user-must-not-be-used");
        let user = text(r.run("display -p '#{uid}|#{user}'"));
        assert_eq!(user, format!("{}|{}\n", rv[2], rv[3]));
        let actual_c_user = text(ccommand(&reference, &["run-shell", "id -u; id -un"]));
        assert_eq!(actual_c_user, format!("{}\n{}\n", cv[2], cv[3]));
        for (position, label) in [
            (0, format!("client-{}", 45002 - index)),
            (1, format!("peer-{}", index)),
            (2, "<account-id>".into()),
            (3, "<account-name>".into()),
        ] {
            native.pairs.push((rv[position].into(), label.clone()));
            c.pairs.push((cv[position].into(), label));
        }
        if index == 0 {
            first_name = rv[0].into();
            cfirst_name = cv[0].into();
        }
    }
    // Replace complete explicit fixture path fields separately, since a drive
    // colon is real path data and must not become a field delimiter.
    let canonical = |map: &Mapping, bytes: &[u8], local_side: bool| {
        let mut text = text(bytes.to_vec());
        for (from, to) in if local_side {
            vec![
                (second_local.to_string(), "<second>"),
                (local.to_string(), "<fixture>"),
            ]
        } else {
            vec![
                (format!("{remote_dir}/second"), "<second>"),
                (remote_dir.clone(), "<fixture>"),
            ]
        } {
            text = text.replace(&from, to);
        }
        map.canonical(text.as_bytes())
    };
    let identity = "#{client_name}|#{client_pid}|#{client_uid}|#{client_user}|#{client_termname}|#{client_termtype}|#{client_termfeatures}|#{client_tty}|#{client_utf8}|#{client_cwd}|#{client_session}|#{client_last_session}";
    let session = "#{session_name}|#{session_path}|#{session_active}|#{session_attached}|#{session_many_attached}|#{session_attached_list}|#{session_group_attached}|#{session_group_attached_list}|#{session_group_many_attached}";
    let window = "#{window_active_clients}|#{window_active_clients_list}|#{window_active_sessions}|#{window_active_sessions_list}|#{window_linked_sessions_list}";
    let tty = fields
        .iter()
        .filter(|f| !matches!(f.as_str(), "client_activity" | "client_created"))
        .map(|f| format!("{f}=#{{{f}}}"))
        .collect::<Vec<_>>()
        .join("|");
    let kinds =
        "#{session_format}#{window_format}#{pane_format}:#{session_id}:#{window_id}:#{pane_id}";
    let nested = format!("{kinds}|#{{L:{kinds};}}");
    for source in [
        format!("display -p '{identity}'"),
        format!("list-sessions -F '{session}'"),
        format!("list-windows -aF '{window}'"),
        format!("display -p '#{{L:{identity};}}'"),
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
        let r = rc.run(&source.replace("client-45002", &first_name));
        let result = cc.run(&source.replace("client-45002", &cfirst_name));
        assert_eq!(
            canonical(&native, &r, true),
            canonical(&c, &result, false),
            "{source}"
        );
    }
    for r in [&mut rc, &mut rsecond] {
        let host = text(r.run("display -p '#{host}|#{host_short}|#{uid}|#{user}'"));
        let fields = host.trim_end_matches('\n').split('|').collect::<Vec<_>>();
        assert_eq!(fields[0].split('.').next().unwrap(), fields[1]);
        assert_eq!(
            fields[0],
            windows_name(windows_sys::Win32::System::WindowsProgramming::GetComputerNameW)
        );
        assert_eq!(fields[2], tmuxx_runtime::windows::account_sid().unwrap());
        assert!(!fields[3].is_empty() && fields[3] != "forged-user-must-not-be-used");
    }
    let c_host = text(cc.run("display -p '#{host}|#{host_short}|#{uid}|#{user}'"));
    let c_identity: serde_json::Value = serde_json::from_slice(&reference.identity()).unwrap();
    let host = c_host.trim_end_matches('\n').split('|').collect::<Vec<_>>();
    assert_eq!(host[0].split('.').next().unwrap(), host[1]);
    assert_eq!(host[0], c_identity["host"][1].as_str().unwrap());
    assert!(!host[2].is_empty() && !host[3].is_empty());
    assert_eq!(
        rc.run("display -p '#{socket_path}'"),
        format!("{}\n", endpoint.display()).as_bytes()
    );
    let c_socket = text(cc.run("display -p '#{socket_path}'"));
    assert_eq!(
        c_socket,
        format!("{}\n", c_identity["socket"].as_str().unwrap())
    );
    assert_eq!(
        rc.run("display -p '#{pid}'"),
        format!("{}\n", std::process::id()).as_bytes()
    );
    assert_eq!(
        text(cc.run("display -p '#{pid}'"))
            .trim()
            .parse::<u64>()
            .unwrap(),
        c_identity["server_pid"].as_u64().unwrap()
    );
    let row_source = format!(
        "list-clients -F '{identity}|#{{session_format}},#{{window_format}},#{{pane_format}}'"
    );
    let rows = canonical(&native, &rc.run(&row_source), true);
    assert_eq!(rows, canonical(&c, &cc.run(&row_source), false));
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
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
        let source = format!(
            "list-clients{order} -F '#{{line}}:{identity}|#{{session_format}},#{{window_format}},#{{pane_format}}'"
        );
        let native_output = canonical(&native, &rc.run(&source), true);
        let c_output = canonical(&c, &cc.run(&source), false);
        if ["name", "index", "modifier", "z", "size", "title"]
            .iter()
            .any(|kind| order.starts_with(&format!(" -O {kind}")))
        {
            // Real OS-assigned names can cross a decimal PID width boundary.
            // Check each exact name-key order against its known fixture names;
            // compare the complete shared row bytes before this assertion.
            for (names, output) in [(&native_names, &native_output), (&c_names, &c_output)] {
                let mut indices = [0, 1];
                indices.sort_by_key(|index| &names[*index]);
                if order.ends_with(" -r") {
                    indices.reverse();
                }
                let expected = indices
                    .into_iter()
                    .enumerate()
                    .filter(|(_, index)| !order.ends_with(" -t b") || *index == 1)
                    .map(|(line, index)| format!("{line}:{}\n", rows[index]))
                    .collect::<String>();
                assert_eq!(output, &expected, "{source}");
            }
        } else {
            assert_eq!(native_output, c_output, "{source}");
        }
    }
    assert_eq!(
        rsecond.run("switch-client -t a"),
        csecond.run("switch-client -t a")
    );
    assert_eq!(
        canonical(
            &native,
            &rsecond.run(&format!("display -p '{identity}'")),
            true
        ),
        canonical(&c, &csecond.run(&format!("display -p '{identity}'")), false)
    );
    assert_eq!(
        canonical(
            &native,
            &rc.run(&format!("list-sessions -F '{session}'")),
            true
        ),
        canonical(&c, &cc.run(&format!("list-sessions -F '{session}'")), false)
    );
    assert_eq!(rc.run("kill-session -t b"), cc.run("kill-session -t b"));
    assert_eq!(
        canonical(
            &native,
            &rsecond.run(&format!("display -p '{identity}'")),
            true
        ),
        canonical(&c, &csecond.run(&format!("display -p '{identity}'")), false)
    );
    let pane = rc.run(&format!(
        "split-window -d -P -F '#{{pane_id}}' {} -u -c {}",
        quote(&python),
        quote("import os,time; os.write(1,b'control-output'); time.sleep(120)")
    ));
    assert_eq!(
        pane,
        cc.run("split-window -d -P -F '#{pane_id}' 'printf control-output; sleep 120'")
    );
    let capture = format!("capture-pane -p -t {}", text(pane).trim());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if rc.run(&capture).starts_with(b"control-output")
            && cc.run(&capture).starts_with(b"control-output")
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        rc.run("display -p '#{client_written}|#{client_discarded}'"),
        b"0|0\n"
    );
    assert_eq!(
        cc.run("display -p '#{client_written}|#{client_discarded}'"),
        b"0|0\n"
    );
    drop((rc, cc, rsecond, csecond));
}
