use rustty::{config::Config, session::Session, tmuxx::{Connection, ServerOptions}};
use serde_json::{json, Value};
use std::{io::{self, Read, Write}, path::Path, sync::{Arc, mpsc, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};

fn title(out: &mut impl Write, value: &str) -> io::Result<()> {
    write!(out, "\x1b]2;{value}\x07")?;
    out.flush()
}
fn child(kind: &str) -> io::Result<()> {
    unsafe {
        let mut termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) != 0 { return Err(io::Error::last_os_error()); }
        libc::cfmakeraw(&mut termios);
        if libc::tcsetattr(0, libc::TCSANOW, &termios) != 0 { return Err(io::Error::last_os_error()); }
    }
    let mut input = io::stdin().lock();
    let mut out = io::stdout().lock();
    title(&mut out, "ready")?;
    let record = "\x1b[1;32mBuild\x1b[22;39m: 日本語 😀 café output abcdefghijklmnopqrstuvwxyz\r\n";
    let payload = if kind == "bulk" { record.repeat((16 * 1024 * 1024) / record.len()) } else { String::new() };
    for index in 0.. {
        let mut byte = [0];
        input.read_exact(&mut byte)?;
        if kind == "bulk" {
            assert_eq!(byte, [b'b']);
            for chunk in payload.as_bytes().chunks(32768 / record.len() * record.len()) {
                out.write_all(chunk)?;
                out.flush()?;
                thread::sleep(Duration::from_millis(4));
            }
            write!(out, "FINAL-OUTPUT-{index}")?;
            title(&mut out, &format!("done-{index}"))?;
        } else {
            assert_eq!(byte, [b'e']);
            title(&mut out, &format!("echo-{index}"))?;
        }
    }
    Ok(())
}
fn wait_title(conn: &Connection, view: &Session, wake: &mpsc::Receiver<()>, expected: &str, timeout: Duration) -> io::Result<()> {
    let until = Instant::now() + timeout;
    loop {
        let errors = conn.errors();
        if !errors.is_empty() { return Err(io::Error::other(format!("{errors:?}"))); }
        if view.terminal()?.title == expected { return Ok(()); }
        if Instant::now() >= until { return Err(io::Error::other(format!("timeout waiting for {expected}"))); }
        let _ = wake.recv_timeout(Duration::from_millis(10));
    }
}
fn command(conn: &Connection, argv: &[&str]) -> io::Result<Vec<u8>> {
    conn.command_with_reply(argv.iter().map(|s| s.as_bytes().to_vec()).collect())?
        .recv_timeout(Duration::from_secs(30)).map_err(io::Error::other)?
}
fn wait_graph(conn: &Connection, count: usize) -> io::Result<Vec<u64>> {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(graph) = conn.graph() {
            if graph.panes.len() >= count { return Ok(graph.panes.iter().map(|p| p.id).collect()); }
        }
        let errors = conn.errors();
        if !errors.is_empty() { return Err(io::Error::other(format!("{errors:?}"))); }
        if Instant::now() >= until { return Err(io::Error::other("graph timeout")); }
        thread::sleep(Duration::from_millis(2));
    }
}
fn retained_rows(view: &Session) -> io::Result<Vec<(u64, String)>> {
    let terminal = view.terminal()?;
    let screen = terminal.screen();
    Ok(screen.all_rows().map(|row| (row.id, screen.row_text(row))).collect())
}
fn sample(observer: &Connection, conn: &Connection, echo: &Session, wake: &mpsc::Receiver<()>, index: &mut usize) -> io::Result<Value> {
    let start = Instant::now();
    assert_eq!(command(observer, &["display-message", "-p", "responsive"] )?, b"responsive\n");
    let command_ns = start.elapsed().as_nanos() as u64;
    let start = Instant::now();
    echo.write(b"e")?;
    wait_title(conn, echo, wake, &format!("echo-{index}"), Duration::from_secs(30))?;
    *index += 1;
    Ok(json!({"command_ns":command_ns,"echo_view_ns":start.elapsed().as_nanos() as u64}))
}
fn run(root: &Path, kind: &str, history: usize, bursts: usize) -> io::Result<Value> {
    std::fs::create_dir(root)?;
    let exe = std::env::current_exe()?;
    let endpoint = root.join("owner.native");
    let config = root.join("owner.conf");
    std::fs::write(&config, format!("set -g history-limit {history}\nset -g default-shell /bin/sh\nset -g remain-on-exit on\nset -g automatic-rename off\n"))?;
    let mut options = ServerOptions::new(&endpoint);
    options.config.push(config);
    let no_wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let owner = Connection::embedded_with_options(options, no_wake.clone(),
        ["-x", "128", "-y", "32", exe.to_str().unwrap(), "--child", "bulk"].map(|s| s.as_bytes().to_vec()).to_vec())?;
    command(&owner, &["new-window", "-d", "-n", "echo", exe.to_str().unwrap(), "--child", "echo"])?;
    let ids = wait_graph(&owner, 2)?;
    let bulk_id = ids[0];
    let echo_id = ids[1];
    let (tx, wake) = mpsc::sync_channel(1);
    let fast_wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || { let _ = tx.try_send(()); });
    let fast = Connection::native(&endpoint, fast_wake.clone())?;
    wait_graph(&fast, 2)?;
    let bulk = Session::attach_tmuxx(&fast, bulk_id, fast_wake.clone())?;
    let echo = Session::attach_tmuxx(&fast, echo_id, fast_wake.clone())?;
    bulk.apply_appearance(&Config::default())?;
    echo.apply_appearance(&Config::default())?;
    wait_title(&fast, &bulk, &wake, "ready", Duration::from_secs(30))?;
    wait_title(&fast, &echo, &wake, "ready", Duration::from_secs(30))?;
    let native_slow = if kind == "native" { Some(Connection::native(&endpoint, no_wake.clone())?) } else { None };
    let slow = native_slow.as_ref().unwrap_or(&owner);
    wait_graph(slow, 2)?;
    let slow_view = (kind != "none").then(|| Session::attach_tmuxx(slow, bulk_id, fast_wake.clone())).transpose()?;
    if let Some(view) = &slow_view { view.apply_appearance(&Config::default())?; wait_title(slow, view, &wake, "ready", Duration::from_secs(30))?; }
    let observer = Connection::native_commands(&endpoint, &fast.instance(), no_wake.clone())?;
    let mut echo_index = 0;
    let mut idle = Vec::new();
    for _ in 0..50 { idle.push(sample(&observer, &fast, &echo, &wake, &mut echo_index)?); thread::sleep(Duration::from_millis(1)); }
    // Holding this guard stops this connection's response parser while its
    // request worker and the owner continue independently.
    let paused = slow_view.as_ref().map(Session::terminal).transpose()?;
    let mut runs = Vec::new();
    for index in 0..bursts {
        let start = Instant::now();
        bulk.write(b"b")?;
        let mut samples = Vec::new();
        while bulk.terminal()?.title != format!("done-{index}") {
            samples.push(sample(&observer, &fast, &echo, &wake, &mut echo_index)?);
            thread::sleep(Duration::from_millis(1));
            if start.elapsed() > Duration::from_secs(30) { return Err(io::Error::other("bulk timeout")); }
        }
        runs.push(json!({"elapsed_ns":start.elapsed().as_nanos() as u64,"samples":samples}));
    }
    let final_title = format!("done-{}", bursts - 1);
    let expected = bulk.terminal()?.plain_text();
    let stopped = AtomicBool::new(false);
    let (recovery, recovery_command_ns) = thread::scope(|scope| -> io::Result<_> {
        let sampling = scope.spawn(|| {
            let mut samples = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                let start = Instant::now();
                if command(&observer, &["display-message", "-p", "recovering"]).is_err() { break; }
                samples.push(start.elapsed().as_nanos() as u64);
                thread::sleep(Duration::from_millis(1));
            }
            samples
        });
        let resume = Instant::now();
        drop(paused);
        let recovery = if let Some(view) = &slow_view {
            match wait_title(slow, view, &wake, &final_title, Duration::from_secs(30)) {
                Ok(()) => json!({"complete":true,"resume_to_final_ns":resume.elapsed().as_nanos() as u64,"equal_text":view.terminal()?.plain_text()==expected}),
                Err(error) => json!({"complete":false,"error":error.to_string()}),
            }
        } else { json!(null) };
        stopped.store(true, Ordering::Release);
        Ok((recovery, sampling.join().unwrap()))
    })?;
    // Full-history verification runs after catch-up latency sampling ends.
    let expected_rows = retained_rows(&bulk)?;
    let recovery_history_equal = if let Some(view) = &slow_view {
        recovery.get("complete").and_then(Value::as_bool) == Some(true)
            && retained_rows(view)? == expected_rows
    } else { true };
    let reconnected = Connection::native(&endpoint, no_wake.clone())?;
    wait_graph(&reconnected, 2)?;
    let view = Session::attach_tmuxx(&reconnected, bulk_id, fast_wake)?;
    view.apply_appearance(&Config::default())?;
    wait_title(&reconnected, &view, &wake, &final_title, Duration::from_secs(30))?;
    let reconnect_equal = view.terminal()?.plain_text() == expected;
    let reconnect_history_equal = retained_rows(&view)? == expected_rows;
    Ok(json!({"schema":1,"slow_transport":kind,"cols":128,"rows":32,"history_lines":history,"bursts":bursts,"command_transport":"separate_native_commands","view_compression":true,"bulk_bytes_approx":16*1024*1024,"bulk_corpus":"styled_mixed","child_batch_bytes_max":32768,"child_batch_sleep_ms":4,"sample_sleep_ms":1,"idle":idle,"pressure":runs,"recovery":recovery,"recovery_command_ns":recovery_command_ns,"reconnect_equal":reconnect_equal,"retained_rows":expected_rows.len(),"recovery_history_equal":recovery_history_equal,"reconnect_history_equal":reconnect_history_equal,"history_verification":"all retained row IDs and text; after latency samples"}))
}
fn main() -> io::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str)==Some("--child") { return child(&args[1]); }
    assert!((2..=4).contains(&args.len()) && ["none", "embedded", "native"].contains(&args[1].as_str()), "pressure NEW_DIR none|embedded|native [HISTORY_LINES] [BURSTS:2|3]");
    let history = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(1024);
    let bursts = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(3);
    assert!([2, 3].contains(&bursts), "BURSTS must be 2 or 3");
    let result = run(Path::new(&args[0]), &args[1], history, bursts);
    match result {
        Ok(value) => { println!("{}", serde_json::to_string_pretty(&value)?); Ok(()) },
        Err(error) => { println!("{}", json!({"complete":false,"error":error.to_string()})); Err(error) }
    }
}
