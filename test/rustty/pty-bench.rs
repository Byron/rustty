//! Real PTY throughput and parser/view-delivery latency; no renderer or GUI.
//! See PTY_BENCHMARKS.md for measurement boundaries and reproduction.
#[cfg(unix)]
mod unix {
    use rustty::{
        config::{Command, Config},
        session::{Session, SessionEvent, SessionOptions},
        tmuxx::{Connection, ServerOptions},
    };
    use serde_json::{Value, json};
    use std::{
        io::{self, Read, Write},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    fn query(
        input: &mut impl Read,
        output: &mut impl Write,
        expected: Option<(u16, u16)>,
    ) -> io::Result<()> {
        output.write_all(b"\x1b[6n")?;
        output.flush()?;
        let mut reply = Vec::new();
        while reply.last() != Some(&b'R') {
            let mut b = [0];
            input.read_exact(&mut b)?;
            reply.push(b[0]);
            assert!(reply.len() < 64, "invalid DSR reply: {reply:?}");
        }
        let position = std::str::from_utf8(
            reply
                .strip_prefix(b"\x1b[")
                .and_then(|v| v.strip_suffix(b"R"))
                .expect("framed DSR reply"),
        )
        .unwrap();
        let (row, col) = position.split_once(';').expect("DSR coordinates");
        let position = (row.parse::<u16>().unwrap(), col.parse::<u16>().unwrap());
        assert!((1..=32).contains(&position.0));
        assert!((1..=128).contains(&position.1));
        if let Some(expected) = expected {
            assert_eq!(position, expected);
        }
        Ok(())
    }
    fn title(output: &mut impl Write, text: &str) -> io::Result<()> {
        write!(output, "\x1b]2;{text}\x07")?;
        output.flush()
    }
    fn child(path: &Path, unpaced: bool) -> io::Result<()> {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        unsafe {
            if libc::tcgetattr(0, termios.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = termios.assume_init();
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let mut input = io::stdin().lock();
        let mut output = io::stdout().lock();
        let mut echo = 0;
        title(&mut output, "ready")?;
        loop {
            let mut b = [0];
            input.read_exact(&mut b)?;
            match b[0] {
                b'q' => return Ok(()),
                b'e' => {
                    title(&mut output, &format!("echo-{echo}"))?;
                    echo += 1;
                }
                b's' => {
                    let mut results = Vec::new();
                    let mut batch_id = 0;
                    let mut samples = Vec::new();
                    for _ in 0..300 {
                        thread::sleep(Duration::from_millis(1));
                        let start = Instant::now();
                        query(&mut input, &mut output, Some((1, 1)))?;
                        samples.push(start.elapsed().as_nanos() as u64);
                    }
                    results.push(json!({"workload":"sparse_dsr", "roundtrip_ns":samples}));
                    for (name, record) in [
                        (
                            "ascii",
                            "ordinary log output: abcdefghijklmnopqrstuvwxyz 0123456789 abcdefghijklmnopqrstuvwxyz 0123456789\r\n",
                        ),
                        (
                            "styled_mixed",
                            "\x1b[1;32mBuild\x1b[22;39m: 日本語 😀 café output abcdefghijklmnopqrstuvwxyz\r\n",
                        ),
                    ] {
                        let payload = record.repeat((16 * 1024 * 1024) / record.len());
                        let mut runs = Vec::new();
                        for _ in 0..3 {
                            let start = Instant::now();
                            // Markers must not split a UTF-8 scalar or escape sequence.
                            let chunk_bytes = if unpaced {
                                payload.len()
                            } else {
                                256 * 1024 / record.len() * record.len()
                            };
                            for chunk in payload.as_bytes().chunks(chunk_bytes) {
                                output.write_all(chunk)?;
                                if !unpaced {
                                    title(&mut output, &format!("batch-{batch_id}"))?;
                                    batch_id += 1;
                                    let mut ack = [0];
                                    input.read_exact(&mut ack)?;
                                    assert_eq!(ack[0], b'a');
                                }
                            }
                            output.flush()?;
                            let written = start.elapsed().as_nanos() as u64;
                            query(&mut input, &mut output, Some((32, 1)))?;
                            runs.push(json!({"write_ns":written, "drained_ns":start.elapsed().as_nanos() as u64}));
                        }
                        results.push(json!({"workload":format!("bulk_{name}"), "bytes":payload.len(), "view_ack_bytes": if unpaced { 0 } else { 256 * 1024 / record.len() * record.len() }, "runs":runs}));
                    }
                    for size in [4096usize, 65536] {
                        let record =
                            "ordinary terminal output 0123456789 abcdefghijklmnopqrstuvwxyz\r\n";
                        let mut payload = record.repeat(size.div_ceil(record.len()));
                        payload.truncate(size);
                        let mut runs = Vec::new();
                        for _ in 0..200 {
                            thread::sleep(Duration::from_millis(1));
                            let start = Instant::now();
                            output.write_all(payload.as_bytes())?;
                            output.flush()?;
                            let written = start.elapsed().as_nanos() as u64;
                            query(&mut input, &mut output, None)?;
                            runs.push(json!({"write_ns":written, "drained_ns":start.elapsed().as_nanos() as u64}));
                        }
                        results.push(json!({"workload":format!("burst_{size}_then_dsr"), "bytes":payload.len(), "runs":runs}));
                    }
                    std::fs::write(path, serde_json::to_vec_pretty(&results)?)?;
                    title(&mut output, "complete")?;
                }
                _ => panic!("unexpected parent input {}", b[0]),
            }
        }
    }
    fn drain(session: &Session) -> io::Result<()> {
        for event in session.events() {
            if let SessionEvent::Error(error) = event {
                return Err(io::Error::other(error));
            }
        }
        Ok(())
    }
    fn wait_title(session: &Session, rx: &mpsc::Receiver<()>, expected: &str) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            drain(session)?;
            if session.terminal()?.title == expected {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("title {expected} timeout"),
                ));
            }
            let _ = rx.recv_timeout(Duration::from_millis(100));
        }
    }
    fn echo(
        session: &Session,
        rx: &mpsc::Receiver<()>,
        first: usize,
        count: usize,
    ) -> io::Result<Vec<u64>> {
        (first..first + count)
            .map(|i| {
                thread::sleep(Duration::from_millis(1));
                while rx.try_recv().is_ok() {}
                let start = Instant::now();
                session.write(b"e")?;
                wait_title(session, rx, &format!("echo-{i}"))?;
                Ok(start.elapsed().as_nanos() as u64)
            })
            .collect()
    }
    fn run(kind: &str, root: &Path, unpaced: bool) -> io::Result<Value> {
        let result_file = root.join(format!("{kind}-child.json"));
        let endpoint = root.join(format!("{kind}.native"));
        let exe = std::env::current_exe()?;
        let pacing = if unpaced { "unpaced" } else { "paced" };
        let (tx, rx) = mpsc::sync_channel(1);
        let wakes = Arc::new(AtomicU64::new(0));
        let coalesced = Arc::new(AtomicU64::new(0));
        let (wake_count, sent_count) = (wakes.clone(), coalesced.clone());
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            wake_count.fetch_add(1, Ordering::Relaxed);
            if tx.try_send(()).is_ok() {
                sent_count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let mut server = None;
        let mut connection = None;
        let mut config = Config::default();
        config.scrollback_limit_lines = Some(1024);
        config.scrollback_limit_bytes = None;
        let session = if kind == "local" {
            Session::spawn(
                &config,
                SessionOptions {
                    cols: 128,
                    rows: 32,
                    command: Some(Command::Direct(vec![
                        exe.to_string_lossy().into_owned(),
                        "--child".into(),
                        result_file.to_string_lossy().into_owned(),
                        pacing.into(),
                    ])),
                    ..Default::default()
                },
                wake.clone(),
            )?
        } else {
            let config_file = root.join(format!("{kind}.conf"));
            std::fs::write(
                &config_file,
                "set -g history-limit 1024\nset -g default-shell /bin/sh\n",
            )?;
            let mut options = ServerOptions::new(&endpoint);
            options.config.push(config_file);
            let initial = [
                "-x",
                "128",
                "-y",
                "32",
                exe.to_str().unwrap(),
                "--child",
                result_file.to_str().unwrap(),
                pacing,
            ]
            .map(|s| s.as_bytes().to_vec())
            .to_vec();
            let conn = if kind == "embedded" {
                Connection::embedded_with_options(options, wake.clone(), initial)?
            } else {
                server = Some(tmuxx_runtime::start(options)?);
                let conn = Connection::native(&endpoint, wake.clone())?;
                let mut argv = vec![
                    b"new-session".to_vec(),
                    b"-d".to_vec(),
                    b"-s".to_vec(),
                    b"bench".to_vec(),
                ];
                argv.extend(initial);
                conn.command_bytes(argv)?;
                conn
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            let pane = loop {
                let errors = conn.errors();
                if !errors.is_empty() {
                    return Err(io::Error::other(format!("connection: {errors:?}")));
                }
                if let Some(graph) = conn.graph()
                    && let Some(pane) = graph.panes.first()
                {
                    break pane.id;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "tmuxx pane creation timeout",
                    ));
                }
                thread::sleep(Duration::from_millis(1));
            };
            let session = Session::attach_tmuxx(&conn, pane, wake.clone())?;
            session.apply_appearance(&config)?;
            connection = Some(conn);
            session
        };
        wait_title(&session, &rx, "ready")?;
        let before_echo = echo(&session, &rx, 0, 300)?;
        std::fs::write(
            root.join(format!("{kind}-echo-before.json")),
            serde_json::to_vec(&before_echo)?,
        )?;
        wakes.store(0, Ordering::Relaxed);
        coalesced.store(0, Ordering::Relaxed);
        session.write(b"s")?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut last_batch = String::new();
        loop {
            drain(&session)?;
            let current = session.terminal()?.title.clone();
            if current.starts_with("batch-") && current != last_batch {
                session.write(b"a")?;
                last_batch = current;
            }
            if result_file.exists() {
                break;
            }
            if let Some(conn) = &connection {
                let errors = conn.errors();
                if !errors.is_empty() {
                    return Err(io::Error::other(format!("connection: {errors:?}")));
                }
            }
            if session.has_exited() {
                return Err(io::Error::other("benchmark child exited"));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "benchmark child timeout",
                ));
            }
            let _ = rx.recv_timeout(Duration::from_millis(20));
        }
        wait_title(&session, &rx, "complete")?;
        let child_results: Value = serde_json::from_slice(&std::fs::read(&result_file)?)?;
        let bulk_wakes = wakes.load(Ordering::Relaxed);
        let bulk_coalesced = coalesced.load(Ordering::Relaxed);
        let after_echo = echo(&session, &rx, 300, 100)?;
        let out = json!({"schema":1,"complete":true,"unpaced":unpaced,"path":kind,"cols":128,"rows":32,"history_lines":1024,"child":child_results,"host_echo_before_ns":before_echo,"host_echo_after_ns":after_echo,"wake_calls":bulk_wakes,"coalesced_wakes":bulk_coalesced});
        session.write(b"q")?;
        drop(session);
        drop(connection);
        drop(server);
        Ok(out)
    }
    pub fn main() -> io::Result<()> {
        let args: Vec<_> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("--child") {
            return child(
                Path::new(&args[2]),
                args.get(3).is_some_and(|arg| arg == "unpaced"),
            );
        }
        let usage = "usage: pty-bench NEW_OUTPUT_DIRECTORY [all|local|embedded|native] [unpaced]";
        let root = PathBuf::from(args.get(1).ok_or_else(|| io::Error::other(usage))?);
        let selected = args.get(2).map_or("all", String::as_str);
        if !["all", "local", "embedded", "native"].contains(&selected)
            || args.len() > 4
            || args.get(3).is_some_and(|value| value != "unpaced")
        {
            return Err(io::Error::other(usage));
        }
        // Never reuse stale child results, sockets, or a previous passing report.
        std::fs::create_dir(&root)?;
        let root = root.canonicalize()?;
        let unpaced = args.get(3).is_some();
        let paths: &[&str] = if selected == "all" {
            &["local", "embedded", "native"]
        } else {
            &[selected]
        };
        for &kind in paths {
            eprintln!("measuring {kind}");
            match run(kind, &root, unpaced) {
                Ok(result) => {
                    std::fs::write(
                        root.join(format!("{kind}.json")),
                        serde_json::to_vec_pretty(&result)?,
                    )?;
                    let samples = result["host_echo_before_ns"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap())
                        .collect::<Vec<_>>();
                    let mut samples = samples;
                    samples.sort_unstable();
                    println!(
                        "{kind}: input-to-parsed-title p50={:.1}us p99={:.1}us",
                        samples[(samples.len() - 1) / 2] as f64 / 1000.0,
                        samples[(samples.len() - 1) * 99 / 100] as f64 / 1000.0
                    );
                    for workload in result["child"].as_array().unwrap() {
                        if workload["workload"].as_str().unwrap().starts_with("bulk_") {
                            let bytes = workload["bytes"].as_u64().unwrap() as f64;
                            let rates: Vec<_> = workload["runs"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|r| {
                                    bytes * 1e9
                                        / r["drained_ns"].as_u64().unwrap() as f64
                                        / 1048576.0
                                })
                                .collect();
                            println!(
                                "  {} MiB/s: {rates:.2?}",
                                workload["workload"].as_str().unwrap()
                            );
                        }
                    }
                }
                Err(error) => {
                    std::fs::write(
                        root.join(format!("{kind}.json")),
                        serde_json::to_vec_pretty(
                            &json!({"schema":1,"path":kind,"complete":false,"unpaced":unpaced,"error":error.to_string()}),
                        )?,
                    )?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::io::Result<()> {
    Err(std::io::Error::other("pty-bench requires a Unix PTY"))
}
