#![cfg(windows)]
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tmuxx_protocol::native::{Request, ServerMessage};
#[path = "../../../conformance/remote_reference.rs"]
mod remote;
use remote::RemoteReference;

fn request(client: &tmuxx_runtime::LocalClient, args: &[&str]) -> Vec<u8> {
    client
        .send(Request {
            request_id: 1,
            argv: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
            stdin: None,
            control: false,
            subscribe: false,
        })
        .unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            assert_eq!(reply.error, None, "{args:?}");
            return reply.output;
        }
        assert!(Instant::now() < end, "native reply {args:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn wait(query: impl Fn() -> Vec<u8>, expected: &[u8]) {
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let actual = query();
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < end,
            "expected {expected:?}, got {actual:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "requires pinned SSH C oracle and packaged native ConPTY"]
fn foreground_path_and_automatic_names_follow_real_processes() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "names-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let directory = std::env::temp_dir().join(format!("tmuxx-{name} 雪 é &"));
    std::fs::create_dir(&directory).unwrap();
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = Directory(directory);
    let source = include_bytes!("../../../conformance/process_names_fixture.py");
    let script = directory.0.join("process_names_fixture.py");
    std::fs::write(&script, source).unwrap();
    let reference = RemoteReference::new();
    let c_script = String::from_utf8(reference.put(b"process_names_fixture.py", source)).unwrap();
    let c_dir = String::from_utf8(reference.directory()).unwrap();
    let c = |args: &[&str]| {
        let output = reference.output(&args.iter().map(|s| s.as_bytes()).collect::<Vec<_>>(), b"");
        assert!(output.status.success(), "C {args:?}: {output:?}");
        output.stdout
    };
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(
        tmuxx_runtime::windows::endpoint(&name).unwrap(),
    ))
    .unwrap();
    let client = host.connect().unwrap();
    let r = |args: &[&str]| request(&client, args);
    let both = |args: &[&str]| {
        assert_eq!(r(args), c(args), "{args:?}");
    };
    let python = PathBuf::from(
        std::env::var_os("TMUXX_REMOTE_PYTHON").expect("absolute native Python fixture executable"),
    );
    assert!(python.is_absolute());
    let native_image = python.file_name().unwrap().to_str().unwrap();
    r(&[
        "new",
        "-d",
        "-s",
        "main",
        "-n",
        "pinned",
        "-c",
        directory.0.to_str().unwrap(),
        python.to_str().unwrap(),
        script.to_str().unwrap(),
    ]);
    c(&[
        "new",
        "-d",
        "-s",
        "main",
        "-n",
        "pinned",
        "-c",
        &c_dir,
        "/usr/bin/python3",
        &c_script,
    ]);
    let first = directory.0.join("first");
    let second = directory.0.join("second");
    let args = [
        "display",
        "-pt",
        "%0",
        "#{pane_current_command}|#{pane_current_path}|#{window_name}",
    ];
    // NT process parameters retain a trailing directory separator.
    let expected = |image: &str, path: &std::path::Path, window: &str| {
        format!("{image}|{}\\|{window}\n", path.display())
    };
    wait(
        || r(&args),
        expected(native_image, &first, "pinned").as_bytes(),
    );
    wait(
        || c(&args),
        format!("python3|{c_dir}/first|pinned\n").as_bytes(),
    );
    let start = ["display", "-pt", "%0", "#{pane_start_path}"];
    assert_eq!(r(&start), format!("{}\n", directory.0.display()).as_bytes());
    assert_eq!(c(&start), format!("{c_dir}\n").as_bytes());
    let tty = ["display", "-pt", "%0", "#{pane_tty}"];
    let capture = ["capture-pane", "-pt", "%0", "-S", "-", "-E", "-"];
    assert_eq!(
        r(&tty),
        b"\n",
        "ConPTY does not manufacture a POSIX tty path"
    );
    assert!(r(&capture).starts_with(b"CONPTY\nREADY"));
    let c_tty = c(&tty);
    assert!(c_tty.starts_with(b"/dev/"));
    assert_eq!(
        c_tty.strip_suffix(b"\n"),
        c(&capture).split(|b| *b == b'\n').next()
    );
    both(&[
        "set",
        "-w",
        "automatic-rename-format",
        "run=#{pane_current_command}",
    ]);
    both(&["set", "-w", "automatic-rename", "on"]);
    wait(
        || r(&args),
        expected(native_image, &first, &format!("run={native_image}")).as_bytes(),
    );
    wait(
        || c(&args),
        format!("python3|{c_dir}/first|run=python3\n").as_bytes(),
    );
    both(&[
        "set-hook",
        "-g",
        "window-renamed",
        "set -g @renamed '#{hook_window_name}'",
    ]);
    both(&[
        "set-hook",
        "-g",
        "window-renamed[1]",
        "set -gF @expanded '#{hook_window_name}'",
    ]);
    both(&["send", "-t", "%0", "go", "Enter"]);
    wait(
        || r(&args),
        expected(native_image, &second, &format!("run={native_image}")).as_bytes(),
    );
    wait(
        || c(&args),
        format!("python3|{c_dir}/second|run=python3\n").as_bytes(),
    );
    both(&["send", "-t", "%0", "go", "Enter"]);
    wait(
        || c(&args),
        format!("sleep|{c_dir}/second|run=sleep\n").as_bytes(),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while !directory.0.join("foreground.pid").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let child_pid = std::fs::read_to_string(directory.0.join("foreground.pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    let child = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child_pid) };
    assert!(!child.is_null());
    struct Process(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Process {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let child = Process(child);
    assert_eq!(unsafe { WaitForSingleObject(child.0, 0) }, WAIT_TIMEOUT);
    assert_eq!(
        r(&args),
        expected(native_image, &second, &format!("run={native_image}")).as_bytes()
    );
    assert_eq!(c(&["show", "-gv", "@renamed"]), b"#{hook_window_name}\n");
    assert_eq!(c(&["show", "-gv", "@expanded"]), b"run=sleep\n");
    assert_eq!(r(&["show", "-gqv", "@renamed"]), b"");
    both(&["renamew", "-t", "main:0", "fixed"]);
    both(&["showw", "-v", "automatic-rename"]);
    both(&["show", "-gv", "@renamed"]);
    both(&["show", "-gv", "@expanded"]);
    both(&["set", "-w", "automatic-rename-format", "ignored"]);
    std::thread::sleep(Duration::from_millis(600));
    both(&["display", "-pt", "%0", "#{window_name}"]);
    assert_eq!(r(&start), format!("{}\n", directory.0.display()).as_bytes());
    assert_eq!(c(&start), format!("{c_dir}\n").as_bytes());
    drop(client);
    host.shutdown().unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(child.0, 5000) },
        windows_sys::Win32::Foundation::WAIT_OBJECT_0,
        "native owner left its foreground descendant alive"
    );
}
