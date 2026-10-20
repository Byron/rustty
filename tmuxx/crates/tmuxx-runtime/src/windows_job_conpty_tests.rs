//! Exercise the production portable-pty path without constructing a runtime pane.
//! The outer fixture job is kept open until every product-ownership assertion.
use super::*;
use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};
use serde_json::{Value, json};
use std::io::Write;
use std::mem::size_of_val;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PREFIX: &str = "TMUXX_CONPTY_TEST:";
const LIMIT: Duration = Duration::from_secs(20);

fn arguments(name: &str, role: &str, directory: &Path) -> Vec<OsString> {
    vec![
        "--exact".into(),
        name.into(),
        "--nocapture".into(),
        "--skip".into(),
        format!("{PREFIX}{role}").into(),
        "--skip".into(),
        directory.as_os_str().to_owned(),
    ]
}

fn child_role() -> Option<(String, PathBuf)> {
    let args = std::env::args_os().collect::<Vec<_>>();
    let values = args
        .windows(2)
        .filter(|pair| pair[0] == "--skip")
        .map(|pair| pair[1].clone())
        .collect::<Vec<_>>();
    let role = values.first()?.to_str()?.strip_prefix(PREFIX)?;
    Some((role.to_owned(), PathBuf::from(&values[1])))
}

fn record(directory: &Path, name: &str, value: Value) {
    let temporary = directory.join(format!("{name}.tmp"));
    std::fs::write(&temporary, serde_json::to_vec(&value).unwrap()).unwrap();
    std::fs::rename(temporary, directory.join(format!("{name}.json"))).unwrap();
}

fn receive(directory: &Path, name: &str) -> Value {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Ok(message) = std::fs::read_to_string(directory.join("panic.txt")) {
            panic!("private ConPTY fixture failed: {message}");
        }
        match std::fs::read(directory.join(format!("{name}.json"))) {
            Ok(bytes) => return serde_json::from_slice(&bytes).unwrap(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("reading {name}: {error}"),
        }
        assert!(Instant::now() < deadline, "timed out awaiting {name}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn try_job_pids(job: HANDLE) -> io::Result<Vec<u32>> {
    #[repr(C)]
    struct Pids {
        assigned: u32,
        count: u32,
        ids: [usize; 64],
    }
    let mut data: Pids = unsafe { zeroed() };
    checked(unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicProcessIdList,
            (&mut data as *mut Pids).cast(),
            size_of::<Pids>() as u32,
            null_mut(),
        )
    })?;
    if data.count > 64 {
        return Err(io::Error::other("too many owned fixture processes"));
    }
    let mut ids = data.ids[..data.count as usize]
        .iter()
        .map(|id| u32::try_from(*id).unwrap())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    Ok(ids)
}

fn job_pids(job: HANDLE) -> Vec<u32> {
    try_job_pids(job).unwrap()
}

fn first_pane_phase(directory: &Path, name: &str) {
    // NULL queries this process's innermost job. The outer cleanup job contains
    // the still-live owner as well, so it cannot satisfy this one-process check.
    // This is the pane program's first test action, before creating descendants.
    let ids = job_pids(null_mut());
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    checked(unsafe {
        QueryInformationJobObject(
            null_mut(),
            JobObjectExtendedLimitInformation,
            (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
            null_mut(),
        )
    })
    .unwrap();
    assert_eq!(ids, vec![std::process::id()]);
    assert_ne!(
        limits.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        0
    );
    record(
        directory,
        name,
        json!({"pid": std::process::id(), "job_pids": ids}),
    );
}

fn sleep_until_killed() -> ! {
    // A failed controller cannot leave a permanently waiting child, even if its
    // outer cleanup boundary regresses. Normal cleanup must beat this lease.
    std::thread::sleep(Duration::from_secs(60));
    std::process::exit(93)
}

type Drain = (std::thread::JoinHandle<()>, mpsc::Receiver<io::Result<()>>);

fn open_conpty() -> (PtyPair, Drain) {
    initialize_fixture_conpty();
    let pair = native_pty_system().openpty(PtySize::default()).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = (|| -> io::Result<()> {
            let mut matched = 0;
            let mut bytes = [0; 4096];
            loop {
                let count = reader.read(&mut bytes)?;
                if count == 0 {
                    return Ok(());
                }
                for &byte in &bytes[..count] {
                    if byte == b"\x1b[6n"[matched] {
                        matched += 1;
                        if matched == 4 {
                            writer.write_all(b"\x1b[1;1R")?;
                            matched = 0;
                        }
                    } else {
                        matched = usize::from(byte == 0x1b);
                    }
                }
            }
        })();
        let _ = tx.send(result);
    });
    (pair, (thread, rx))
}

fn initialize_fixture_conpty() {
    let resources = std::env::var_os("TMUXX_CONPTY_RESOURCES")
        .expect("TMUXX_CONPTY_RESOURCES must name the verified shipped resources");
    super::super::initialize_conpty(Path::new(&resources)).unwrap();
}

fn close_conpty(pair: PtyPair, (thread, done): Drain) {
    drop(pair);
    done.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    thread.join().unwrap();
}

fn pane_command(name: &str, role: &str, directory: &Path) -> CommandBuilder {
    let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
    command.args(arguments(name, role, directory));
    command
}

fn handle_count() -> u32 {
    let mut count = 0;
    checked(unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }).unwrap();
    count
}

fn run_child(name: &str, role: &str, directory: &Path) {
    let panic_path = directory.join("panic.txt");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |message| {
        let _ = std::fs::write(&panic_path, message.to_string());
        previous(message);
    }));
    match role {
        "owner" => {
            let (pair, _drain) = open_conpty();
            let child = pair
                .slave
                .spawn_command(pane_command(name, "pane", directory))
                .unwrap();
            record(
                directory,
                "owner",
                json!({"pane": child.process_id().unwrap()}),
            );
            // No Runtime, Process, pane registry or reaper has been constructed.
            // Keep both returned handles alive, and let only the OS reclaim them.
            sleep_until_killed();
        }
        "pane" => {
            first_pane_phase(directory, "pane");
            receive(directory, "spawn-descendants");
            let exe = std::env::current_exe().unwrap();
            for (role, flags) in [("foreground", 0), ("background", CREATE_NEW_PROCESS_GROUP)] {
                #[expect(
                    clippy::zombie_processes,
                    reason = "the external controller kills this owner and verifies every descendant through held process handles"
                )]
                let _child = Command::new(&exe)
                    .args(arguments(name, role, directory))
                    .creation_flags(flags | CREATE_NO_WINDOW)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
            }
            sleep_until_killed();
        }
        "retry" => {
            first_pane_phase(directory, "retry");
            sleep_until_killed();
        }
        "foreground" | "background" | "unrelated" => {
            record(directory, role, json!({"pid": std::process::id()}));
            sleep_until_killed();
        }
        "failed-owner" => {
            use windows_sys::Win32::System::Diagnostics::Debug::*;
            initialize_fixture_conpty();
            let process_mode = unsafe { GetErrorMode() };
            record(directory, "owner-baseline", json!({}));
            receive(directory, "baseline-held");
            // An existing invalid PE reaches CreateProcessW; a missing filename
            // could be rejected by CommandBuilder without testing Windows cleanup.
            let executable = directory.join("invalid.exe");
            std::fs::write(&executable, b"private fixture: not a Windows executable").unwrap();
            let mut counts = Vec::new();
            for (cycle, mode) in [0, SEM_NOOPENFILEERRORBOX, SEM_NOGPFAULTERRORBOX]
                .into_iter()
                .enumerate()
            {
                let (pair, drain) = open_conpty();
                record(directory, &format!("opened-{cycle}"), json!({}));
                receive(directory, &format!("attempt-failure-{cycle}"));
                checked(unsafe { SetThreadErrorMode(mode, null_mut()) }).unwrap();
                let error = pair
                    .slave
                    .spawn_command(CommandBuilder::new(&executable))
                    .unwrap_err();
                let message = error.to_string();
                assert!(
                    message.contains("CreateProcessW")
                        && (message.contains("os error 193") || message.contains("os error 216")),
                    "{message}"
                );
                assert_eq!(unsafe { GetThreadErrorMode() }, mode);
                assert_eq!(unsafe { GetErrorMode() }, process_mode);
                record(directory, &format!("failed-{cycle}"), json!({"mode": mode}));
                receive(directory, &format!("drop-failed-{cycle}"));
                close_conpty(pair, drain);
                record(directory, &format!("closed-{cycle}"), json!({}));
                // The controller holds each host process handle and confirms
                // its exit before we measure the owner's retained handles.
                receive(directory, &format!("host-gone-{cycle}"));
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut count = handle_count();
                let mut stable_since = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_millis(10));
                    let next = handle_count();
                    if next != count {
                        count = next;
                        stable_since = Instant::now();
                    }
                    if stable_since.elapsed() >= Duration::from_millis(100) {
                        break;
                    }
                    assert!(Instant::now() < deadline, "owner handles never settled");
                }
                counts.push(count);
            }
            // The first ConPTY initializes process-wide Windows runtime state.
            // Subsequent complete failed-start cycles must not grow it.
            assert_eq!(
                counts[1], counts[2],
                "failed startup handle counts: {counts:?}"
            );
            record(directory, "cycles", json!({"post_cleanup_handles": counts}));
            receive(directory, "retry-spawn");
            let (pair, drain) = open_conpty();
            let child = pair
                .slave
                .spawn_command(pane_command(name, "retry", directory))
                .unwrap();
            assert_eq!(unsafe { GetThreadErrorMode() }, SEM_NOGPFAULTERRORBOX);
            assert_eq!(unsafe { GetErrorMode() }, process_mode);
            record(
                directory,
                "retry-owner",
                json!({"pane": child.process_id().unwrap()}),
            );
            receive(directory, "drop-retry");
            drop(child);
            close_conpty(pair, drain);
        }
        other => panic!("unknown private fixture role {other}"),
    }
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("tmuxx-conpty-spawn-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        // The fixture writes only immediate files: never traverse a directory,
        // junction or any recursively discovered path during cleanup.
        if let Ok(entries) = std::fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

struct Fixture(OwnedStdioChild);
impl Fixture {
    fn new(name: &str, role: &str, directory: &Path) -> Self {
        Self(
            spawn_owned_stdio_piped_stderr(
                &std::env::current_exe().unwrap(),
                &arguments(name, role, directory),
            )
            .unwrap(),
        )
    }
    fn job(&self) -> HANDLE {
        self.0.child.job.0.as_raw_handle()
    }
    fn held_processes(&self) -> Vec<(u32, OwnedHandle)> {
        job_pids(self.job())
            .into_iter()
            .map(|pid| {
                let handle = unsafe {
                    OpenProcess(
                        PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                        0,
                        pid,
                    )
                };
                assert!(!handle.is_null(), "cannot hold live fixture process {pid}");
                let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
                let mut image = [0u16; 1024];
                let mut length = image.len() as u32;
                checked(unsafe {
                    QueryFullProcessImageNameW(
                        handle.as_raw_handle(),
                        0,
                        image.as_mut_ptr(),
                        &mut length,
                    )
                })
                .unwrap();
                println!(
                    "held ConPTY fixture process {pid}: {}",
                    String::from_utf16(&image[..length as usize]).unwrap()
                );
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
                    WAIT_TIMEOUT
                );
                (pid, handle)
            })
            .collect()
    }
    fn owner_only_exit(&self) {
        checked(unsafe { TerminateProcess(self.0.child.process.as_raw_handle(), 91) }).unwrap();
    }
    fn assert_empty(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !job_pids(self.job()).is_empty() {
            assert!(Instant::now() < deadline, "owned fixture processes remain");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        while try_job_pids(self.job()).is_ok_and(|ids| !ids.is_empty()) && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn assert_dead(processes: &[(u32, OwnedHandle)]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    for (pid, handle) in processes {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u32;
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), remaining) },
            WAIT_OBJECT_0,
            "process {pid} escaped ConPTY owner cleanup"
        );
    }
}

#[test]
fn first_child_phase_and_descendants_are_owned_before_runtime_registration() {
    const NAME: &str = "windows_job::conpty_tests::first_child_phase_and_descendants_are_owned_before_runtime_registration";
    if let Some((role, directory)) = child_role() {
        return run_child(NAME, &role, &directory);
    }
    let directory = Directory::new();
    let mut unrelated = Fixture::new(NAME, "unrelated", &directory.0);
    receive(&directory.0, "unrelated");
    let fixture = Fixture::new(NAME, "owner", &directory.0);
    let owner = receive(&directory.0, "owner");
    let first = receive(&directory.0, "pane");
    assert_eq!(owner["pane"], first["pid"]);
    assert_eq!(first["job_pids"], json!([first["pid"]]));
    let initial = fixture.held_processes();
    assert!(
        initial
            .iter()
            .any(|(pid, _)| Some(u64::from(*pid)) == first["pid"].as_u64())
    );
    record(&directory.0, "spawn-descendants", json!({}));
    let foreground = receive(&directory.0, "foreground");
    let background = receive(&directory.0, "background");
    let owned = fixture.held_processes();
    for marker in [&foreground, &background] {
        assert!(
            owned
                .iter()
                .any(|(pid, _)| Some(u64::from(*pid)) == marker["pid"].as_u64())
        );
    }
    assert!(
        owned.len() >= 5,
        "expected owner, OpenConsole, pane and two descendants"
    );
    fixture.owner_only_exit();
    assert_dead(&initial);
    assert_dead(&owned);
    fixture.assert_empty();
    assert!(
        unrelated.0.try_wait().unwrap().is_none(),
        "independent process was killed"
    );
    unrelated.0.kill().unwrap();
    unrelated.0.wait().unwrap();
    unrelated.assert_empty();
}

#[test]
fn failed_spawn_reclaims_conpty_host_and_allows_fresh_spawn() {
    const NAME: &str =
        "windows_job::conpty_tests::failed_spawn_reclaims_conpty_host_and_allows_fresh_spawn";
    if let Some((role, directory)) = child_role() {
        return run_child(NAME, &role, &directory);
    }
    let directory = Directory::new();
    let mut fixture = Fixture::new(NAME, "failed-owner", &directory.0);
    let owner = fixture.0.id();
    receive(&directory.0, "owner-baseline");
    let baseline = fixture.held_processes();
    let baseline_pids = baseline.iter().map(|(pid, _)| *pid).collect::<Vec<_>>();
    assert!(baseline_pids.contains(&owner));
    record(&directory.0, "baseline-held", json!({}));
    for (cycle, mode) in [0, 32768, 2].into_iter().enumerate() {
        receive(&directory.0, &format!("opened-{cycle}"));
        let initial = fixture.held_processes();
        assert!(
            initial.len() > baseline.len(),
            "ConPTY did not create its OpenConsole host"
        );
        record(&directory.0, &format!("attempt-failure-{cycle}"), json!({}));
        assert_eq!(
            receive(&directory.0, &format!("failed-{cycle}"))["mode"],
            mode
        );
        assert_eq!(
            job_pids(fixture.job()),
            initial.iter().map(|(pid, _)| *pid).collect::<Vec<_>>()
        );
        record(&directory.0, &format!("drop-failed-{cycle}"), json!({}));
        receive(&directory.0, &format!("closed-{cycle}"));
        let hosts = initial
            .into_iter()
            .filter(|(pid, _)| !baseline_pids.contains(pid))
            .collect::<Vec<_>>();
        assert_dead(&hosts);
        assert_eq!(job_pids(fixture.job()), baseline_pids);
        assert!(fixture.0.try_wait().unwrap().is_none());
        record(&directory.0, &format!("host-gone-{cycle}"), json!({}));
    }
    let cycles = receive(&directory.0, "cycles");
    println!("ConPTY complete failed-start cycles: {cycles}");
    record(&directory.0, "retry-spawn", json!({}));
    let retry_owner = receive(&directory.0, "retry-owner");
    let retry = receive(&directory.0, "retry");
    assert_eq!(retry_owner["pane"], retry["pid"]);
    let retry_processes = fixture.held_processes();
    assert!(retry_processes.len() >= 3);
    record(&directory.0, "drop-retry", json!({}));
    assert_dead(&retry_processes);
    assert!(fixture.0.wait().unwrap().success());
    fixture.assert_empty();
}
