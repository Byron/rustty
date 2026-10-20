#![cfg(windows)]

#[path = "../../../conformance/remote_reference.rs"]
#[allow(dead_code)] // This target uses the explicit startup constructor.
mod remote_reference;
use remote_reference::{RemoteControl, RemoteReference};
use std::{
    io::{Read, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

const EXE: &str = env!("CARGO_BIN_EXE_tmuxx");

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "configuration startup lifetime deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Pending(Child);
impl Pending {
    fn kill(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
    fn finish(mut self) -> Output {
        let mut stdout = self.0.stdout.take().unwrap();
        let mut stderr = self.0.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let err = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        until(|| self.0.try_wait().unwrap().is_some());
        Output {
            status: self.0.wait().unwrap(),
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn remote_finish(child: RemoteControl) -> Output {
    until(|| child.exited().is_some());
    let code = child.exited().unwrap();
    let mut output = child.read();
    output.status = ExitStatus::from_raw(code as u32);
    output
}

fn success(output: Output, expected: &[u8]) -> Vec<u8> {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, expected, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    output.stdout
}

fn powershell(script: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let value = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | (chunk.get(2).copied().unwrap_or(0) as u32);
        for i in 0..4 {
            encoded.push(if i > chunk.len() {
                '='
            } else {
                ALPHABET[((value >> (18 - 6 * i)) & 63) as usize] as char
            });
        }
    }
    let system = std::env::var("SystemRoot").unwrap();
    format!(
        "\"{system}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoProfile -NonInteractive -EncodedCommand {encoded}"
    )
}

struct Native {
    directory: PathBuf,
    label: String,
    owner: Option<Pending>,
    panes: Vec<OwnedHandle>,
}
impl Native {
    fn new() -> Self {
        let label = format!("config-lifetime-{}", std::process::id());
        let directory = std::env::temp_dir().join(&label);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("empty"), []).unwrap();
        let background = powershell(
            "$ErrorActionPreference='Stop'; [System.IO.File]::WriteAllText('background','')",
        );
        let gate = powershell(
            "$ErrorActionPreference='Stop'; [System.IO.File]::WriteAllText('ready',''); $until=[DateTime]::UtcNow.AddSeconds(45); while (!(Test-Path -LiteralPath 'go')) { if ([DateTime]::UtcNow -gt $until) {exit 91}; Start-Sleep -Milliseconds 10 }",
        );
        let quote = |value: &str| {
            String::from_utf8(tmuxx_command::quote_argument(value.as_bytes())).unwrap()
        };
        let config = format!(
            "set -ga @runs x\nnew-session -d -s keep python -c 'import time; time.sleep(120)'\nrun-shell -b {}\nrun-shell {}\nsource-file {}\nset -g @late 1\n",
            quote(&background),
            quote(&gate),
            quote(directory.join("empty").to_str().unwrap())
        );
        let config_path = directory.join("conf");
        std::fs::write(&config_path, config).unwrap();
        let owner = Pending(
            Command::new(EXE)
                .args(["-D", "-L", &label, "-f"])
                .arg(&config_path)
                .current_dir(&directory)
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        Self {
            directory,
            label,
            owner: Some(owner),
            panes: Vec::new(),
        }
    }
    fn spawn(&self, args: &[&str]) -> Pending {
        Pending(
            Command::new(EXE)
                .args(["-L", &self.label])
                .args(args)
                .current_dir(&self.directory)
                .env_remove("TMUX")
                .env_remove("TMUXX_ENDPOINT")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }
    fn watch_panes(&mut self) {
        let output = self
            .spawn(&["list-panes", "-a", "-F", "#{pane_pid}"])
            .finish();
        assert!(output.status.success(), "{output:?}");
        for line in String::from_utf8(output.stdout).unwrap().lines() {
            let pid = line.parse::<u32>().unwrap();
            let handle =
                unsafe { windows_sys::Win32::System::Threading::OpenProcess(0x100000, 0, pid) };
            assert!(!handle.is_null(), "cannot observe pane {pid}");
            self.panes
                .push(unsafe { OwnedHandle::from_raw_handle(handle) });
        }
    }
    fn close(&mut self) {
        if let Some(mut owner) = self.owner.take() {
            owner.kill();
        }
        for pane in &self.panes {
            assert_eq!(
                unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(
                        pane.as_raw_handle(),
                        5000,
                    )
                },
                0,
                "native config fixture leaked pane process"
            );
        }
        self.panes.clear();
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        let _ = std::fs::write(self.directory.join("go"), []);
        if let Some(mut owner) = self.owner.take() {
            let _ = owner.0.kill();
            let _ = owner.0.wait();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn reject_native_connections(label: &str) {
    let endpoint = tmuxx_runtime::windows::endpoint(label).unwrap();
    for _ in 0..140 {
        let mut stream = tmuxx_runtime::windows::connect(&endpoint).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        tmuxx_runtime::native_handshake(&mut stream).unwrap();
        stream
            .write_all(b"{\"request_id\":1,\"argv\":null}\n")
            .unwrap();
        match stream.read(&mut [0; 1]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ) => {}
            result => {
                panic!("malformed identified native startup peer retained its slot: {result:?}")
            }
        }
    }
}

#[test]
#[ignore = "requires pinned real C oracle and native pinned ConPTY"]
fn configuration_loads_once_after_starter_loss_and_waiters_observe_completion() {
    let source=b"set -ga @runs x\nnew-session -d -s keep 'sleep 120'\nrun-shell -b 'touch background'\nrun-shell 'touch ready; while [ ! -f go ]; do sleep .01; done'\nsource-file /dev/null\nset -g @late 1\n";
    let reference = RemoteReference::new_with_configuration(source);
    until(|| reference.exists(b"ready") && reference.exists(b"background"));
    let starter = reference.startup_client();
    assert!(starter.exited().is_none());
    starter.close();
    std::thread::sleep(Duration::from_millis(100));
    let lost = reference.command_open_args(&[b"display", b"-p", b"lost"]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        lost.exited().is_none(),
        "C lost client passed startup barrier"
    );
    lost.close();
    reference.malformed_startup_clients();
    let waiting = reference.command_open_args(&[
        b"new-session",
        b"-d",
        b"-s",
        b"second",
        b"sleep 120",
        b";",
        b"display",
        b"-p",
        b"#{@runs} #{@late}",
    ]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        waiting.exited().is_none(),
        "C waiter passed startup barrier"
    );
    reference.put(b"go", &[]);
    let c = [
        success(remote_finish(waiting), b"x 1\n"),
        success(
            reference.output(&[b"list-sessions", b"-F", b"#{session_name}"], &[]),
            b"keep\nsecond\n",
        ),
        success(
            reference.output(&[b"display", b"-p", b"#{@runs} #{@late}"], &[]),
            b"x 1\n",
        ),
    ];

    let mut native = Native::new();
    until(|| {
        native.directory.join("ready").exists() && native.directory.join("background").exists()
    });
    // Windows ownership is explicit. Losing the initiating command must never
    // kill, replace, or reload the separate standalone owner.
    let mut starter = native.spawn(&["start-server", ";", "display", "-p", "starter"]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(starter.0.try_wait().unwrap().is_none());
    starter.kill();
    let mut lost = native.spawn(&["display", "-p", "lost"]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        lost.0.try_wait().unwrap().is_none(),
        "native lost client passed startup barrier"
    );
    lost.kill();
    reject_native_connections(&native.label);
    let mut waiting = native.spawn(&[
        "new-session",
        "-d",
        "-s",
        "second",
        "python",
        "-c",
        "import time; time.sleep(120)",
        ";",
        "display",
        "-p",
        "#{@runs} #{@late}",
    ]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        waiting.0.try_wait().unwrap().is_none(),
        "native waiter passed startup barrier"
    );
    std::fs::write(native.directory.join("go"), []).unwrap();
    let actual = [
        success(waiting.finish(), b"x 1\n"),
        success(
            native
                .spawn(&["list-sessions", "-F", "#{session_name}"])
                .finish(),
            b"keep\nsecond\n",
        ),
        success(
            native
                .spawn(&["display", "-p", "#{@runs} #{@late}"])
                .finish(),
            b"x 1\n",
        ),
    ];
    assert_eq!(actual, c);
    native.watch_panes();
    assert!(
        native
            .owner
            .as_mut()
            .unwrap()
            .0
            .try_wait()
            .unwrap()
            .is_none()
    );
    native.close();
}
