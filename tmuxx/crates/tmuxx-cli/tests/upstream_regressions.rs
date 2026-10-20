//! Run the upstream files in place. Their assertions and normalization belong
//! to upstream; this runner does not rewrite scripts, commands, or output.
#![cfg(unix)]

use std::{
    cell::Cell,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, FileTypeExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

// Linux fixtures get a private session so nested process groups and reparented
// descendants remain attributable after the script exits. Other platforms keep
// their existing process-group contract.
fn isolate_script(command: &mut Command) -> &mut Command {
    #[cfg(target_os = "linux")]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
    }
    #[cfg(not(target_os = "linux"))]
    command.process_group(0)
}

#[cfg(target_os = "linux")]
fn kill_script_session(session: u32) {
    // The caller has not reaped the leader, reserving its session/PID identity.
    // Pin each member before checking membership again; never signal recycled
    // numeric PIDs or groups. A deliberate independent setsid session is outside
    // this ownership boundary.
    for _ in 0..3 {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            break;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if descriptor < 0 {
                continue;
            }
            let process = unsafe { File::from_raw_fd(descriptor as i32) };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some((_, fields)) = stat.rsplit_once(") ") else {
                continue;
            };
            if fields
                .split_whitespace()
                .nth(3)
                .and_then(|value| value.parse::<u32>().ok())
                != Some(session)
            {
                continue;
            }
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    process.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct ScriptChild(Child, bool);

impl ScriptChild {
    fn new(child: Child) -> Self {
        Self(child, false)
    }

    fn exited(&self) -> io::Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // Keep the leader unreaped so its process-group ID cannot be recycled.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.0.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn finish(&mut self) -> io::Result<ExitStatus> {
        if !self.1 {
            self.1 = true;
            #[cfg(target_os = "linux")]
            kill_script_session(self.0.id());
            // Non-Linux owners inherit this group; their watchdog cleans PTYs.
            #[cfg(not(target_os = "linux"))]
            unsafe {
                libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL);
            }
        }
        self.0.wait()
    }
}

impl Drop for ScriptChild {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn bounded_wait(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

fn process_start(pid: libc::pid_t) -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as libc::c_int;
        if unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        } != size
            || info.pbi_status == libc::SZOMB
        {
            return None;
        }
        Some((info.pbi_start_tvsec, info.pbi_start_tvusec))
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields = stat
            .rsplit_once(") ")?
            .1
            .split_whitespace()
            .collect::<Vec<_>>();
        if fields.first() == Some(&"Z") {
            return None;
        }
        Some((fields.get(19)?.parse().ok()?, 0))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

fn kill_matching_process(pid: libc::pid_t, started: (u64, u64)) {
    #[cfg(target_os = "linux")]
    {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if descriptor < 0 {
            return;
        }
        let process = unsafe { File::from_raw_fd(descriptor as i32) };
        if process_start(pid) == Some(started) {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    process.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    if process_start(pid) == Some(started) {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

fn socket_owner(path: &Path) -> Option<(libc::pid_t, (u64, u64))> {
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() {
        return None;
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, &byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = byte as libc::c_char;
    }
    #[cfg(target_os = "macos")]
    {
        address.sun_len = std::mem::size_of_val(&address) as u8;
    }
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if descriptor < 0 {
        return None;
    }
    let socket = unsafe { UnixStream::from_raw_fd(descriptor) };
    socket.set_nonblocking(true).ok()?;
    // A full listen backlog must not turn best-effort ownership lookup into a wait.
    if unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    } != 0
    {
        return None;
    }
    #[cfg(target_os = "macos")]
    let pid = {
        let mut pid: libc::pid_t = 0;
        let mut size = std::mem::size_of_val(&pid) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut size,
            )
        } != 0
        {
            return None;
        }
        pid
    };
    #[cfg(target_os = "linux")]
    let pid = {
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&peer) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut peer as *mut libc::ucred).cast(),
                &mut size,
            )
        } != 0
        {
            return None;
        }
        peer.pid
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let pid = {
        let _ = socket;
        return None;
    };
    if pid <= 1 || pid == std::process::id() as libc::pid_t {
        return None;
    }
    Some((pid, process_start(pid)?))
}

struct Run {
    directory: PathBuf,
    binary: PathBuf,
    retain: Cell<bool>,
}

impl Run {
    fn new(binary: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(format!(
            "/tmp/tx-upstream-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        Self {
            directory,
            binary: binary.to_owned(),
            retain: Cell::new(false),
        }
    }

    fn script(&self, script: &str) -> (bool, String) {
        self.script_with_timeout(script, Duration::from_secs(120))
    }

    fn script_with_timeout(&self, script: &str, timeout: Duration) -> (bool, String) {
        let output_path = self.directory.join("transcript");
        let output = File::create(&output_path).unwrap();
        let mut child = ScriptChild::new(
            isolate_script(
                Command::new("/bin/sh")
                    .arg(script)
                    .current_dir(
                        PathBuf::from(
                            std::env::var_os("TMUXX_C_SOURCE").expect("set TMUXX_C_SOURCE"),
                        )
                        .join("regress"),
                    )
                    .env_remove("TMUX")
                    .env_remove("TMUXX_ENDPOINT")
                    .env("TEST_TMUX", &self.binary)
                    .env("TMUX_TMPDIR", &self.directory)
                    .env("TMPDIR", &self.directory)
                    .env("SHELL", "/bin/sh")
                    .env("LC_ALL", "C.UTF-8")
                    .stdin(Stdio::null())
                    .stdout(output.try_clone().unwrap())
                    .stderr(output),
            )
            .spawn()
            .unwrap(),
        );
        let deadline = Instant::now() + timeout;
        loop {
            if child.exited().unwrap() {
                let status = child.finish().unwrap();
                self.retain.set(!status.success());
                drop(child);
                return (
                    status.success(),
                    std::fs::read_to_string(output_path).unwrap(),
                );
            }
            if Instant::now() >= deadline {
                self.retain.set(true);
                drop(child);
                panic!(
                    "{} {script} timed out:\n{}",
                    self.binary.display(),
                    std::fs::read_to_string(output_path).unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // Most scripts already kill their owner. Clean sockets left by failed
        // assertions as well, without touching the user's ordinary socket root.
        let socket_directory = self
            .directory
            .join(format!("tmux-{}", unsafe { libc::getuid() }));
        if let Ok(entries) = std::fs::read_dir(socket_directory) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !entry.file_type().is_ok_and(|kind| kind.is_socket())
                    || name.as_bytes().ends_with(b".native")
                    || name.as_bytes().ends_with(b".lock")
                {
                    continue;
                }
                let owner = socket_owner(&entry.path());
                if let Ok(mut child) = Command::new(&self.binary)
                    .arg("-S")
                    .arg(entry.path())
                    .args(["-N", "kill-server"])
                    .env_remove("TMUX")
                    .env_remove("TMUXX_ENDPOINT")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    bounded_wait(&mut child, Duration::from_secs(2));
                }
                if let Some((pid, started)) = owner {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while process_start(pid) == Some(started) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // Revalidate the start identity after acquiring a Linux pidfd.
                    kill_matching_process(pid, started);
                }
            }
        }
        if self.retain.get() {
            eprintln!(
                "upstream transcript: {}",
                self.directory.join("transcript").display()
            );
        } else {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

#[test]
#[ignore = "requires real Unix sockets and PTYs"]
fn harness_cleans_flooding_owners_after_script_exit_and_timeout() {
    #[cfg(target_os = "linux")]
    if let Some(pids) = std::env::var_os("TMUXX_TEST_ABRUPT_HARNESS") {
        let run = Run::new(Path::new(env!("CARGO_BIN_EXE_tmuxx")));
        if std::env::var_os("TMUXX_TEST_ABRUPT_EMBEDDED").is_some() {
            use std::io::Write;
            // Force the collector to use SIGKILL on the embedding host. Its
            // broker must still get time to kill an env-cleared, HUP-ignoring pane.
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
            let host =
                tmuxx_runtime::start(tmuxx_runtime::Options::new(run.directory.join("embedded")))
                    .unwrap();
            let client = host.connect().unwrap();
            let python = "import os,signal,sys,time\nsignal.signal(signal.SIGHUP,signal.SIG_IGN)\nwith open(sys.argv[1],'a') as log: log.write('pane %s\\nbroker %s\\n' % (os.getpid(),os.getppid()))\nwhile True: time.sleep(1)";
            client
                .send(tmuxx_protocol::native::Request {
                    request_id: 1,
                    argv: [
                        "new-session",
                        "-d",
                        "-s",
                        "embedded",
                        "/usr/bin/env",
                        "-i",
                        "/usr/bin/python3",
                        "-c",
                        python,
                        pids.to_str().unwrap(),
                    ]
                    .map(|argument| argument.as_bytes().to_vec())
                    .to_vec(),
                    stdin: None,
                    subscribe: false,
                    control: false,
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(tmuxx_protocol::native::ServerMessage::Reply(reply)) = client.try_recv() {
                    assert_eq!(reply.error, None);
                    break;
                }
                assert!(Instant::now() < deadline, "embedded spawn timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&pids)
                .unwrap();
            writeln!(log, "owner {}", std::process::id()).unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        let script = run.directory.join("outer-timeout.sh");
        std::fs::write(&script, r#"
"$TEST_TMUX" -Labrupt -f/dev/null new-session -d -s abrupt /usr/bin/env -i /usr/bin/python3 -c 'import os,signal,sys,time
signal.signal(signal.SIGHUP, signal.SIG_IGN)
with open(sys.argv[1], "a") as log:
 log.write("pane %s\nbroker %s\n" % (os.getpid(), os.getppid()))
while True: time.sleep(1)' "$TMUXX_TEST_ABRUPT_HARNESS" || exit 90
owner=$("$TEST_TMUX" -Labrupt display -p '#{pid}') || exit 91
printf 'owner %s\n' "$owner" >> "$TMUXX_TEST_ABRUPT_HARNESS"
sleep 60
"#).unwrap();
        assert!(Path::new(&pids).parent().unwrap().is_dir());
        run.script_with_timeout(script.to_str().unwrap(), Duration::from_secs(60));
        return;
    }
    if let Some(path) = std::env::var_os("TMUXX_TEST_UNRESPONSIVE_SOCKET") {
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut native = path.clone();
        native.push(".native");
        let _native = std::os::unix::net::UnixListener::bind(native).unwrap();
        let mut connections = Vec::new();
        for stream in listener.incoming() {
            connections.push(stream.unwrap());
        }
        return;
    }
    let binary = Path::new(env!("CARGO_BIN_EXE_tmuxx"));
    let independent = Run::new(binary);
    let endpoint = independent.directory.join("independent.native");
    let owner = ScriptChild::new(
        isolate_script(
            Command::new(binary)
                .args(["-D", "-f", "/dev/null", "--native-endpoint"])
                .arg(&endpoint)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .spawn()
        .unwrap(),
    );
    let owner_pid = owner.0.id() as libc::pid_t;
    let owner_start = process_start(owner_pid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&endpoint).is_err() {
        assert!(Instant::now() < deadline, "independent owner did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    for ending in ["exit 0", "exit 1", "sleep 60"] {
        let run = Run::new(binary);
        let script = run.directory.join("forced-cleanup.sh");
        #[cfg(target_os = "linux")]
        let nested = {
            std::fs::write(
                run.directory.join("nested.py"),
                r#"import os, signal, time
signal.signal(signal.SIGHUP, signal.SIG_IGN)
os.setpgid(0, 0)
with open(os.environ['TMUX_TMPDIR'] + '/nested-pids', 'a') as log:
    print(os.getpid(), file=log, flush=True)
if os.fork() == 0:
    os.setpgid(0, 0)
    parent = os.getpid()
    if os.fork():
        os._exit(0)
    while os.getppid() == parent:
        time.sleep(0.01)
    with open(os.environ['TMUX_TMPDIR'] + '/nested-pids', 'a') as log:
        print(os.getpid(), file=log, flush=True)
while True:
    time.sleep(1)
"#,
            )
            .unwrap();
            r#"python3 "$TMUX_TMPDIR/nested.py" &
n=0
while [ ! -f "$TMUX_TMPDIR/nested-pids" ] || [ "$(wc -l < "$TMUX_TMPDIR/nested-pids")" -ne 2 ]; do
    n=$((n + 1))
    [ "$n" -lt 200 ] || exit 93
    sleep 0.01
done
"#
        };
        #[cfg(not(target_os = "linux"))]
        let nested = "";
        #[cfg(target_os = "linux")]
        let cleared = r#"
"$TEST_TMUX" -Lleak split-window -d /usr/bin/env -i /usr/bin/python3 -c 'import os,signal,sys,time
signal.signal(signal.SIGHUP,signal.SIG_IGN)
with open(sys.argv[1], "w") as log: log.write("%s\n%s\n" % (os.getpid(),os.getppid()))
while True: time.sleep(1)' "$TMUX_TMPDIR/cleared-pids" || exit 94
n=0
while [ ! -f "$TMUX_TMPDIR/cleared-pids" ] || [ "$(wc -l < "$TMUX_TMPDIR/cleared-pids")" -ne 2 ]; do
    n=$((n + 1))
    [ "$n" -lt 200 ] || exit 95
    sleep 0.01
done
"#;
        #[cfg(not(target_os = "linux"))]
        let cleared = "";
        std::fs::write(&script, format!(r#"
"$TEST_TMUX" -Lleak -f /dev/null new -d -s flood 'mkfifo "$TMUX_TMPDIR/flood"; cat /dev/zero > "$TMUX_TMPDIR/flood" & printf "%s\n" "$$" "$!" > "$TMUX_TMPDIR/pane-pids"; tr "\000" x < "$TMUX_TMPDIR/flood" & echo "$!" >> "$TMUX_TMPDIR/pane-pids"; wait' || exit 90
"$TEST_TMUX" -Lleak display -p '#{{pid}}' > "$TMUX_TMPDIR/owner-pid" || exit 91
n=0
while [ ! -f "$TMUX_TMPDIR/pane-pids" ] || [ "$(wc -l < "$TMUX_TMPDIR/pane-pids")" -ne 3 ]; do
    n=$((n + 1))
    [ "$n" -lt 200 ] || exit 92
    sleep 0.01
done
{nested}
{cleared}
{ending}
"#)).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run.script_with_timeout(
                script.to_str().unwrap(),
                Duration::from_secs(if ending == "sleep 60" { 2 } else { 10 }),
            )
        }));
        if ending == "sleep 60" {
            assert!(result.is_err(), "fixture did not time out");
        } else {
            let (success, transcript) = result.unwrap();
            assert_eq!(success, ending == "exit 0", "{transcript}");
        }
        #[cfg(target_os = "linux")]
        let pid_files = ["owner-pid", "pane-pids", "nested-pids", "cleared-pids"];
        #[cfg(not(target_os = "linux"))]
        let pid_files = ["owner-pid", "pane-pids"];
        let pids = pid_files
            .iter()
            .map(|name| std::fs::read_to_string(run.directory.join(name)).unwrap())
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .map(|pid| pid.parse::<libc::pid_t>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            pids.len(),
            if cfg!(target_os = "linux") { 8 } else { 4 },
            "owner, pane shell, cat, tr and Linux nested/orphan and env-cleared pane/broker"
        );
        // Preserve exact identities for emergency cleanup if the assertion fails.
        struct Remaining(Vec<(libc::pid_t, (u64, u64))>);
        impl Drop for Remaining {
            fn drop(&mut self) {
                for &(pid, started) in &self.0 {
                    kill_matching_process(pid, started);
                }
            }
        }
        let _remaining = Remaining(
            pids.iter()
                .filter_map(|&pid| Some((pid, process_start(pid)?)))
                .collect(),
        );
        run.retain.set(false);
        drop(run);
        let deadline = Instant::now() + Duration::from_secs(5);
        while pids.iter().any(|&pid| process_start(pid).is_some()) {
            assert!(
                Instant::now() < deadline,
                "{ending} left fixture processes {pids:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(process_start(owner_pid), Some(owner_start));
        let mut query = Command::new(binary)
            .arg("--native-endpoint")
            .arg(&endpoint)
            .args(["display", "-p", "independent"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        bounded_wait(&mut query, Duration::from_secs(2));
        let output = query.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"independent\n");
    }
    #[cfg(target_os = "linux")]
    for embedded in [false, true] {
        // The outer collector kills this nested Rust test before its Run/Child
        // destructors execute. Its private script session and broker inherit
        // the collector marker, while the HUP-ignoring pane clears its env.
        let outer = Run::new(binary);
        let pids = outer.directory.join("outer-pids");
        let transcript = File::create(outer.directory.join("transcript")).unwrap();
        let python = r#"import os, subprocess, sys
sys.path.insert(0, os.environ['TMUXX_CONFORMANCE_DIRECTORY'])
import runner
try:
    runner.run_suite_process([os.environ['TMUXX_ABRUPT_EXECUTABLE'], '--ignored', '--exact',
        'harness_cleans_flooding_owners_after_script_exit_and_timeout'],
        cwd=os.getcwd(), env=dict(os.environ), timeout=2)
except subprocess.TimeoutExpired as error:
    assert not error.cleanup_errors, error.cleanup_errors
    print('outer timeout cleaned private script, owner, broker and env-cleared pane')
else:
    raise AssertionError('nested harness did not time out')
"#;
        let mut command = Command::new("python3");
        if embedded {
            command.env("TMUXX_TEST_ABRUPT_EMBEDDED", "1");
        }
        let mut child = command
            .args(["-c", python])
            .env(
                "TMUXX_CONFORMANCE_DIRECTORY",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance"),
            )
            .env("TMUXX_ABRUPT_EXECUTABLE", std::env::current_exe().unwrap())
            .env("TMUXX_TEST_ABRUPT_HARNESS", &pids)
            .stdout(transcript.try_clone().unwrap())
            .stderr(transcript)
            .spawn()
            .unwrap();
        bounded_wait(&mut child, Duration::from_secs(16));
        let status = child.wait().unwrap();
        let transcript = std::fs::read_to_string(outer.directory.join("transcript")).unwrap();
        assert!(status.success(), "{transcript}");
        let records = std::fs::read_to_string(pids).unwrap();
        let pids = records
            .lines()
            .map(|line| {
                let (kind, pid) = line.split_once(' ').unwrap();
                (kind, pid.parse::<libc::pid_t>().unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(pids.len(), 3, "{records}");
        let deadline = Instant::now() + Duration::from_secs(3);
        while pids.iter().any(|(_, pid)| process_start(*pid).is_some()) {
            assert!(
                Instant::now() < deadline,
                "outer timeout leaked {pids:?}; {transcript}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(process_start(owner_pid), Some(owner_start));
    }
    // A detached owner that never answers must not trap the destructor.
    let unresponsive = Run::new(binary);
    let sockets = unresponsive
        .directory
        .join(format!("tmux-{}", unsafe { libc::getuid() }));
    std::fs::create_dir(&sockets).unwrap();
    let socket = sockets.join("unresponsive");
    let stalled = ScriptChild::new(
        isolate_script(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "harness_cleans_flooding_owners_after_script_exit_and_timeout",
                ])
                .env("TMUXX_TEST_UNRESPONSIVE_SOCKET", &socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .spawn()
        .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "stalled fixture did not listen");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        socket_owner(&socket).unwrap().0,
        stalled.0.id() as libc::pid_t
    );
    let started = Instant::now();
    drop(unresponsive);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !stalled.exited().unwrap() {
        assert!(
            Instant::now() < deadline,
            "verified peer fallback did not stop owner"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "shutdown was not bounded"
    );
    assert_eq!(process_start(stalled.0.id() as libc::pid_t), None);
    assert_eq!(process_start(owner_pid), Some(owner_start));
}

fn reference() -> PathBuf {
    std::env::var_os("TMUXX_C_TMUX")
        .expect("set TMUXX_C_TMUX to current C tmux")
        .into()
}

fn passes(script: &str) {
    for binary in [reference(), PathBuf::from(env!("CARGO_BIN_EXE_tmuxx"))] {
        let run = Run::new(&binary);
        let (success, transcript) = run.script(script);
        assert!(success, "{} {script}:\n{transcript}", binary.display());
    }
}

macro_rules! regression {
    ($name:ident, $script:literal) => {
        #[test]
        #[ignore = "requires current C reference and PTY/socket access"]
        fn $name() {
            passes($script);
        }
    };
}

regression!(upstream_input_cursor, "input-cursor.sh");
regression!(upstream_input_edit, "input-edit.sh");
regression!(upstream_input_lines_region, "input-lines-region.sh");
regression!(upstream_input_modes, "input-modes.sh");
regression!(upstream_input_malformed, "input-malformed.sh");
regression!(upstream_input_scroll, "input-scroll.sh");
regression!(upstream_input_sgr, "input-sgr.sh");
regression!(upstream_input_unicode, "input-unicode.sh");
regression!(upstream_input_osc, "input-osc.sh");
regression!(upstream_format_fuzzy, "format-fuzzy.sh");
regression!(upstream_format_modifiers, "format-modifiers.sh");
regression!(
    upstream_format_no_current_window,
    "format-no-current-window.sh"
);
regression!(upstream_format_time_range, "format-time-range.sh");
regression!(upstream_json, "json.sh");
regression!(upstream_buffers, "buffers.sh");
regression!(upstream_set_hook_b, "set-hook-B.sh");
regression!(upstream_targets, "targets.sh");
regression!(upstream_targets_panes, "targets-panes.sh");
regression!(upstream_targets_current, "targets-current.sh");
regression!(upstream_layout_nesting_limit, "layout-nesting-limit.sh");
regression!(upstream_control_client_exit, "control-client-exit.sh");
regression!(
    upstream_control_client_exit_stalled,
    "control-client-exit-stalled.sh"
);
regression!(
    upstream_control_client_run_shell_exit,
    "control-client-run-shell-exit.sh"
);
regression!(
    upstream_control_client_wait_exit,
    "control-client-wait-exit.sh"
);
regression!(upstream_control_notify_guard, "control-notify-guard.sh");
regression!(upstream_control_subscriptions, "control-subscriptions.sh");

#[test]
#[ignore = "requires current C reference and PTY/socket access"]
fn upstream_format_strings_requires_excluded_copy_mode() {
    let script = "format-strings.sh";
    let (success, transcript) = Run::new(&reference()).script(script);
    assert!(success, "C {script}:\n{transcript}");
    let (success, transcript) = Run::new(Path::new(env!("CARGO_BIN_EXE_tmuxx"))).script(script);
    assert!(
        !success,
        "original format-strings unexpectedly passed; review its native UI scope"
    );
    assert_eq!(
        transcript,
        "unsupported command: copy-mode\nFormat test failed for '#{?pane_in_mode,abc}'.\nExpected: 'abc'\nBut got   ''\n"
    );
}

#[test]
#[ignore = "requires current C reference and PTY/socket access"]
fn upstream_raw_diagnostic_differences_remain_explicit() {
    for (script, expected_failures) in [
        (
            "input-raw-controls.sh",
            vec!["FAIL: charset raw missing"; 3]
                .into_iter()
                .chain(["FAIL: g1charset raw missing"])
                .collect::<Vec<_>>(),
        ),
        ("input-nowrap-collect.sh", vec!["FAIL: padding raw missing"]),
    ] {
        let (success, transcript) = Run::new(&reference()).script(script);
        assert!(success, "C {script}:\n{transcript}");
        let (success, transcript) = Run::new(Path::new(env!("CARGO_BIN_EXE_tmuxx"))).script(script);
        assert!(
            !success,
            "{script} unexpectedly passed; retire its representation difference"
        );
        let failures = transcript
            .lines()
            .filter(|line| line.starts_with("FAIL:"))
            .collect::<Vec<_>>();
        assert_eq!(failures, expected_failures, "{script}:\n{transcript}");
        assert!(
            !transcript.contains("tmuxx:"),
            "unexpected command failure: {transcript}"
        );
    }
}
