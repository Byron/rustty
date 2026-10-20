//! Native Job ownership versus the genuine C POSIX session boundary.
#[path = "../../../../conformance/remote_reference.rs"]
mod remote_reference;

use super::{command, endpoint};
use remote_reference::RemoteReference;
use std::{
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{Foundation::WAIT_TIMEOUT, System::Threading::*};

struct Fixture {
    host: Option<tmuxx_runtime::ServerHandle>,
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
struct Unrelated(Child);
impl Drop for Unrelated {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Process(OwnedHandle);
impl Process {
    fn new(pid: u32) -> Self {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        assert!(!handle.is_null(), "open test-owned process {pid}");
        Self(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_TIMEOUT }
    }
    fn exited(&self) {
        assert_eq!(
            unsafe { WaitForSingleObject(self.0.as_raw_handle(), 3000) },
            0
        );
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.alive() {
            unsafe { TerminateProcess(self.0.as_raw_handle(), 1) };
            self.exited();
        }
    }
}

fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "ownership fixture exceeded its deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn pty_descendant_cleanup_difference_matches_c() {
    let python = std::env::var("TMUXX_REMOTE_PYTHON").expect("explicit native Python executable");
    let mut unrelated = Unrelated(
        Command::new(&python)
            .args(["-c", "import time; time.sleep(120)"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for detached in [false, true] {
        // Keep both original POSIX categories: ignored-SIGHUP setpgid and setsid.
        // The helper owns all surviving descendants through its private lease.
        let reference = RemoteReference::new();
        let pidfile = reference.path(b"descendant.pid");
        let release = reference.path(b"release");
        let source = format!(
            "import os,pathlib,signal,sys,time\npid=os.fork()\nif pid == 0:\n {}\n signal.signal(signal.SIGHUP,signal.SIG_IGN)\n destination=pathlib.Path(sys.argv[1]); temporary=destination.with_name(destination.name+'.tmp'); temporary.write_text(str(os.getpid()),encoding='ascii'); temporary.replace(destination)\n os.write(1,b'READY')\n time.sleep(60)\nelse:\n while not os.path.exists(sys.argv[2]): time.sleep(.01)\n os._exit(0)",
            if detached {
                "os.setsid()"
            } else {
                "os.setpgid(0,0)"
            }
        );
        let output = reference.output(
            &[
                b"new-session",
                b"-d",
                b"-s",
                b"ownership",
                b"python3",
                b"-c",
                source.as_bytes(),
                &pidfile,
                &release,
            ],
            &[],
        );
        assert!(output.status.success(), "{output:?}");
        wait(|| reference.exists(b"descendant.pid"));
        let pid = String::from_utf8(reference.get(b"descendant.pid"))
            .unwrap()
            .parse()
            .unwrap();
        let child = reference.watch_process(pid);
        let output = reference.output(
            &[
                b"display-message",
                b"-p",
                b"-t",
                b"ownership",
                b"#{pane_pid}",
            ],
            &[],
        );
        assert!(output.status.success(), "{output:?}");
        let leader = reference.watch_process(
            String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        assert!(reference.process_alive(child));
        reference.put(b"release", b"exit");
        wait(|| !reference.process_alive(leader));
        assert!(
            reference.process_alive(child),
            "C descendant must survive leader exit: detached={detached}"
        );
        let killed = reference.output(&[b"kill-server"], &[]);
        assert!(killed.status.success(), "{killed:?}");
        wait(|| !reference.server_alive());
        assert!(
            reference.process_alive(child),
            "C ignored-SIGHUP descendant must survive server exit: detached={detached}"
        );
        drop(reference);

        let directory = std::env::temp_dir().join(format!(
            "tmuxx-ownership-{}-{}-{}",
            std::process::id(),
            detached,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = endpoint();
        let mut fixture = Fixture {
            host: Some(tmuxx_runtime::start(tmuxx_runtime::Options::new(&path)).unwrap()),
            directory,
        };
        let pidfile = fixture.directory.join("descendant.pid");
        let release = fixture.directory.join("release");
        // DETACHED_PROCESS changes console/session attachment but cannot escape
        // the owner's Job. No child inherits test communication handles.
        let source = format!(
            "import pathlib,subprocess,sys,time\nchild=subprocess.Popen([sys.executable,'-c',\"import ctypes,os,pathlib,signal,sys,time; signal.signal(signal.SIGINT,signal.SIG_IGN); signal.signal(signal.SIGBREAK,signal.SIG_IGN); pathlib.Path(sys.argv[1]).write_text(str(os.getpid())+'|'+str(ctypes.windll.kernel32.GetConsoleCP())); time.sleep(60)\",sys.argv[1]],creationflags={},close_fds=True,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\nwhile not pathlib.Path(sys.argv[2]).exists(): time.sleep(.01)",
            if detached {
                "subprocess.DETACHED_PROCESS"
            } else {
                "subprocess.CREATE_NEW_PROCESS_GROUP"
            }
        );
        assert_eq!(
            command(
                &path,
                &[
                    "new-session",
                    "-d",
                    "-s",
                    "ownership",
                    &python,
                    "-c",
                    &source,
                    pidfile.to_str().unwrap(),
                    release.to_str().unwrap()
                ]
            )
            .error,
            None
        );
        wait(|| std::fs::read_to_string(&pidfile).is_ok_and(|text| text.contains('|')));
        let text = std::fs::read_to_string(&pidfile).unwrap();
        let (pid, console) = text.split_once('|').unwrap();
        assert_eq!(
            console.parse::<u32>().unwrap() == 0,
            detached,
            "actual child console attachment"
        );
        let child = Process::new(pid.parse().unwrap());
        let leader = Process::new(
            String::from_utf8(command(&path, &["display-message", "-p", "#{pane_pid}"]).output)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        std::fs::write(release, b"exit").unwrap();
        leader.exited();
        assert!(
            child.alive(),
            "child must remain until owner shutdown: detached={detached}"
        );
        fixture.host.take().unwrap().shutdown().unwrap();
        child.exited();
        assert!(
            unrelated.0.try_wait().unwrap().is_none(),
            "owner cleanup terminated an unrelated process"
        );
    }
}
