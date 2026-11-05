//! Real PTY owner storage/access probe. Metadata observers never subscribe.
use rustty::{config::Config, session::Session, tmuxx::Connection};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tmuxx_protocol::native::{Request, ServerMessage};
use tmuxx_runtime::{LocalClient, ServerHandle};

fn rss_bytes() -> Option<usize> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            let value = line
                .strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<usize>()
                .ok()?;
            value.checked_mul(1024)
        })
}
fn error(value: impl std::fmt::Display) -> io::Error {
    io::Error::other(value.to_string())
}
fn title(out: &mut impl Write, value: &str) -> io::Result<()> {
    write!(out, "\x1b]2;{value}\x07")?;
    out.flush()
}
fn line(row: usize, kind: &str) -> String {
    if kind == "ascii" {
        format!(
            "line={row:06} file=src/terminal.rs task={:04} result=complete\r\n",
            row % 1000
        )
    } else {
        format!(
            "\x1b[1;32mBuild\x1b[22;39m: line={row:06} 日本語 😀 café output abcdefghijklmnopqrstuvwxyz\r\n"
        )
    }
}
fn corpus(rows: usize, kind: &str) -> Vec<u8> {
    let mut result = Vec::new();
    for row in 0..rows {
        result.extend_from_slice(line(row, kind).as_bytes());
    }
    result
}
fn child(rows: usize, kind: &str) -> io::Result<()> {
    unsafe {
        let mut settings = std::mem::zeroed();
        if libc::tcgetattr(0, &mut settings) != 0 {
            return Err(io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut settings);
        if libc::tcsetattr(0, libc::TCSANOW, &settings) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let data = corpus(rows, kind);
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    title(&mut output, "ready")?;
    let mut echo = 0;
    loop {
        let mut command = [0];
        input.read_exact(&mut command)?;
        match command[0] {
            b'b' => {
                for chunk in data.chunks(32768) {
                    output.write_all(chunk)?;
                }
                title(&mut output, "done")?;
            }
            b'e' => {
                let start = Instant::now();
                output.write_all(b"\x1b[6n")?;
                output.flush()?;
                let mut response = Vec::new();
                loop {
                    input.read_exact(&mut command)?;
                    response.push(command[0]);
                    if command[0] == b'R' {
                        break;
                    }
                    if response.len() > 64 {
                        return Err(error("invalid DSR reply"));
                    }
                }
                if !response.starts_with(b"\x1b[") {
                    return Err(error("invalid DSR prefix"));
                }
                title(
                    &mut output,
                    &format!("echo-{echo}-{}", start.elapsed().as_nanos()),
                )?;
                echo += 1;
            }
            b'q' => return Ok(()),
            value => return Err(error(format!("unexpected input {value}"))),
        }
    }
}
enum Observer {
    Local(LocalClient, u64),
    Native(Connection),
}
impl Observer {
    fn command(&mut self, argv: &[&str]) -> io::Result<Vec<u8>> {
        let argv = argv.iter().map(|value| value.as_bytes().to_vec()).collect();
        match self {
            Self::Native(connection) => connection
                .command_with_reply(argv)?
                .recv_timeout(Duration::from_secs(30))
                .map_err(error)?,
            Self::Local(client, serial) => {
                *serial += 1;
                client.sender().send_wait(Request {
                    request_id: *serial,
                    argv,
                    stdin: None,
                    subscribe: false,
                    control: false,
                })?;
                loop {
                    match client
                        .recv_timeout(Duration::from_secs(30))
                        .map_err(error)?
                    {
                        ServerMessage::Reply(reply) if reply.request_id == *serial => {
                            if let Some(message) = reply.error {
                                return Err(error(message));
                            }
                            return Ok(reply.output);
                        }
                        ServerMessage::Snapshot { .. } | ServerMessage::PaneOutput { .. } => {
                            return Err(error("metadata observer unexpectedly subscribed"));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    fn metadata(&mut self, pane: &str) -> io::Result<(String, usize, usize)> {
        let bytes = self.command(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_title}|#{history_bytes}|#{history_size}",
        ])?;
        let text = String::from_utf8(bytes).map_err(error)?;
        let parts = text.trim().split('|').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(error(format!("invalid metadata {text:?}")));
        }
        Ok((
            parts[0].into(),
            parts[1].parse().map_err(error)?,
            parts[2].parse().map_err(error)?,
        ))
    }
    fn wait_title(&mut self, pane: &str, expected: &str) -> io::Result<()> {
        let start = Instant::now();
        while self.metadata(pane)?.0 != expected {
            if start.elapsed() > Duration::from_secs(30) {
                return Err(error("owner title timeout"));
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
    fn settle(&mut self, pane: &str) -> io::Result<Value> {
        let start = Instant::now();
        let mut changed = Instant::now();
        let mut previous = None;
        let mut samples = Vec::new();
        loop {
            let (_, bytes, rows) = self.metadata(pane)?;
            if previous != Some(bytes) {
                previous = Some(bytes);
                changed = Instant::now();
            }
            samples.push(
                json!({"ns":start.elapsed().as_nanos(),"owned_bytes":bytes,"history_rows":rows}),
            );
            if changed.elapsed() >= Duration::from_millis(100) {
                return Ok(
                    json!({"elapsed_ns":start.elapsed().as_nanos(),"owned_bytes":bytes,"history_rows":rows,"process_rss_bytes":rss_bytes(),"samples":samples}),
                );
            }
            if start.elapsed() > Duration::from_secs(5) {
                return Err(error("owner memory did not settle"));
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}
fn wait_graph(connection: &Connection) -> io::Result<()> {
    let start = Instant::now();
    while connection.graph().is_none() {
        let errors = connection.errors();
        if !errors.is_empty() {
            return Err(error(format!("{errors:?}")));
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err(error("native graph timeout"));
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}
fn echo_sample(
    observer: &mut Observer,
    pane: &str,
    index: usize,
    view: Option<&Session>,
) -> io::Result<Value> {
    let start = Instant::now();
    assert_eq!(
        observer.command(&["display-message", "-p", "responsive"])?,
        b"responsive\n"
    );
    let command_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    if let Some(view) = view {
        view.write(b"e")?;
    } else {
        observer.command(&["send-keys", "-l", "-t", pane, "e"])?;
    }
    let prefix = format!("echo-{index}-");
    let title = loop {
        let title = if let Some(view) = view {
            view.terminal()?.title.clone()
        } else {
            observer.metadata(pane)?.0
        };
        if title.starts_with(&prefix) {
            break title;
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err(error("echo timeout"));
        }
        thread::sleep(Duration::from_micros(100));
    };
    Ok(
        json!({"command_ns":command_ns,"echo_observed_ns":start.elapsed().as_nanos(),"child_dsr_ns":title[prefix.len()..].parse::<u64>().map_err(error)?}),
    )
}
fn run(directory: &Path, mode: &str, rows: usize, kind: &str) -> io::Result<Value> {
    let input_bytes: usize = (0..rows).map(|row| line(row, kind).len()).sum();
    std::fs::create_dir(directory)?;
    let exe = std::env::current_exe()?;
    let endpoint = directory.join("owner.native");
    let config = directory.join("owner.conf");
    std::fs::write(
        &config,
        format!(
            "set -g history-limit {}\nset -g default-shell /bin/sh\nset -g remain-on-exit on\nset -g automatic-rename off\n",
            rows + 100
        ),
    )?;
    let mut options = tmuxx_runtime::Options::new(&endpoint);
    options.config.push(config);
    let server: ServerHandle = tmuxx_runtime::start(options)?;
    let mut observer = Observer::Local(server.connect()?, 0);
    let rows_text = rows.to_string();
    let bulk = String::from_utf8(observer.command(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "probe",
        "-x",
        "128",
        "-y",
        "32",
        exe.to_str().unwrap(),
        "--child",
        &rows_text,
        kind,
    ])?)
    .map_err(error)?
    .trim()
    .to_owned();
    let echo = String::from_utf8(observer.command(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        exe.to_str().unwrap(),
        "--child",
        "0",
        "echo",
    ])?)
    .map_err(error)?
    .trim()
    .to_owned();
    observer.wait_title(&bulk, "ready")?;
    observer.wait_title(&echo, "ready")?;
    let no_wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let remote = if mode == "remote" {
        let c = Connection::native(&endpoint, no_wake.clone())?;
        wait_graph(&c)?;
        Some(c)
    } else {
        None
    };
    let views = if let Some(remote) = &remote {
        let bulk_view =
            Session::attach_tmuxx(remote, bulk[1..].parse().map_err(error)?, no_wake.clone())?;
        let echo_view =
            Session::attach_tmuxx(remote, echo[1..].parse().map_err(error)?, no_wake.clone())?;
        bulk_view.apply_appearance(&Config::default())?;
        echo_view.apply_appearance(&Config::default())?;
        observer = Observer::Native(Connection::native_commands(
            &endpoint,
            &remote.instance(),
            no_wake.clone(),
        )?);
        Some((bulk_view, echo_view))
    } else {
        None
    };
    let mut echo_index = 0;
    let mut idle = Vec::new();
    for _ in 0..20 {
        idle.push(echo_sample(
            &mut observer,
            &echo,
            echo_index,
            views.as_ref().map(|v| &v.1),
        )?);
        echo_index += 1;
    }
    let start = Instant::now();
    observer.command(&["send-keys", "-l", "-t", &bulk, "b"])?;
    observer.wait_title(&bulk, "done")?;
    let feed_ns = start.elapsed().as_nanos();
    // These may already be quiescent: production can compress between PTY bursts.
    let mut maintenance = Vec::new();
    for _ in 0..40 {
        maintenance.push(echo_sample(
            &mut observer,
            &echo,
            echo_index,
            views.as_ref().map(|v| &v.1),
        )?);
        echo_index += 1;
        thread::sleep(Duration::from_micros(500));
    }
    let initial = observer.settle(&bulk)?;
    let mut capture_observer = Observer::Local(server.connect()?, 0);
    let capture_done = AtomicBool::new(false);
    let capture_start = Instant::now();
    let ((capture, capture_ns), capture_activity) = thread::scope(|scope| -> io::Result<_> {
        let capture_task = scope.spawn(|| {
            let start = Instant::now();
            let result = capture_observer.command(&["capture-pane", "-p", "-S", "-", "-t", &bulk]);
            let elapsed = start.elapsed().as_nanos();
            capture_done.store(true, Ordering::Release);
            result.map(|bytes| (bytes, elapsed))
        });
        let mut activity = Vec::new();
        let mut last_bytes = None;
        let mut last_change = Instant::now();
        loop {
            let at_ns = capture_start.elapsed().as_nanos();
            let sample = echo_sample(
                &mut observer,
                &echo,
                echo_index,
                views.as_ref().map(|v| &v.1),
            )?;
            echo_index += 1;
            let (_, bytes, _) = observer.metadata(&bulk)?;
            if last_bytes != Some(bytes) {
                last_bytes = Some(bytes);
                last_change = Instant::now();
            }
            activity.push(json!({"at_ns": at_ns, "owner_bytes_after": bytes, "sample": sample}));
            if capture_done.load(Ordering::Acquire)
                && last_change.elapsed() >= Duration::from_millis(100)
            {
                break;
            }
            if capture_start.elapsed() > Duration::from_secs(30) {
                return Err(error("capture/reclaim activity timeout"));
            }
            thread::sleep(Duration::from_micros(500));
        }
        Ok((
            capture_task
                .join()
                .map_err(|_| error("capture worker panicked"))??,
            activity,
        ))
    })?;
    if !capture
        .windows(b"line=000123".len())
        .any(|s| s == b"line=000123")
    {
        return Err(error("capture missing old history"));
    }
    let after_capture = observer.settle(&bulk)?;
    let start = Instant::now();
    let snapshot = Connection::native(&endpoint, no_wake.clone())?;
    wait_graph(&snapshot)?;
    let attached = Session::attach_tmuxx(&snapshot, bulk[1..].parse().map_err(error)?, no_wake)?;
    let snapshot_ns = start.elapsed().as_nanos();
    let old_row_present = {
        let terminal = attached.terminal()?;
        terminal
            .screen()
            .all_rows()
            .take(1000)
            .any(|row| terminal.screen().row_text(row).contains("line=000123"))
    };
    if !old_row_present {
        return Err(error("snapshot missing old history"));
    }
    drop(attached);
    drop(snapshot);
    let after_snapshot = observer.settle(&bulk)?;
    let result = json!({"schema":1,"mode":mode,"rows":rows,"kind":kind,"cols":128,"screen_rows":32,"input_bytes":input_bytes,"feed_to_owner_done_ns":feed_ns,"initial_idle":idle,"post_feed_samples":maintenance,"capture_activity":capture_activity,"initial_memory":initial,"cold_capture_ns":capture_ns,"capture_bytes":capture.len(),"after_capture":after_capture,"cold_native_attach_ns":snapshot_ns,"after_snapshot":after_snapshot,"echo_boundary":if mode=="remote"{"parsed attached view"}else{"owner title observed through command polling"},"rendered_latency":false});
    drop(views);
    drop(remote);
    drop(observer);
    server.shutdown()?;
    Ok(result)
}
fn main() -> io::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--child") {
        return child(args[1].parse().map_err(error)?, &args[2]);
    }
    if args.len() != 4 || !matches!(args[1].as_str(), "detached" | "remote") {
        return Err(error(
            "owner-probe NEW_DIR detached|remote ROWS ascii|unicode",
        ));
    }
    let result = run(
        Path::new(&args[0]),
        &args[1],
        args[2].parse().map_err(error)?,
        &args[3],
    )?;
    println!("{}", serde_json::to_string(&result).map_err(error)?);
    Ok(())
}
