//! Raw grid diagnostics compare common fields and preserve separate exact
//! examples for native storage differences; no transcript normalization.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
#[cfg(unix)]
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use tmuxx_terminal::{CaptureOptions, Terminal};

struct Reference {
    #[cfg(unix)]
    binary: PathBuf,
    #[cfg(unix)]
    directory: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
}
impl Reference {
    fn start(bytes: &[u8], columns: u16, rows: u16) -> Self {
        #[cfg(unix)]
        let reference = {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "tx-raw-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self {
                binary: std::env::var_os("TMUXX_C_TMUX")
                    .expect("set TMUXX_C_TMUX")
                    .into(),
                directory,
            }
        };
        #[cfg(windows)]
        let reference = Self {
            remote: remote_reference::RemoteReference::new(),
        };
        let payload = bytes
            .iter()
            .map(|b| format!("\\{b:03o}"))
            .collect::<String>();
        reference.command(&[
            "new-session",
            "-d",
            "-x",
            &columns.to_string(),
            "-y",
            &rows.to_string(),
            &format!("stty raw -echo; printf '{payload}'; exec sleep 60"),
        ]);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !reference
            .command(&["capture-pane", "-p", "-S", "-"])
            .windows(5)
            .any(|w| w == b"READY")
        {
            assert!(Instant::now() < deadline, "raw fixture not ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        reference
    }
    fn command(&self, arguments: &[&str]) -> Vec<u8> {
        #[cfg(windows)]
        let output = self.remote.output(
            &arguments
                .iter()
                .map(|arg| arg.as_bytes())
                .collect::<Vec<_>>(),
            &[],
        );
        #[cfg(unix)]
        let output = Command::new(&self.binary)
            .args(["-f", "/dev/null", "-S"])
            .arg(self.directory.join("s"))
            .args(arguments)
            .env_remove("TMUX")
            .env("TERM", "screen")
            .env("LC_ALL", "C.UTF-8")
            .output()
            .unwrap();
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        output.stdout
    }
    fn raw(&self) -> String {
        String::from_utf8(self.command(&["capture-pane", "-pR"])).unwrap()
    }
}
#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .arg("-S")
            .arg(self.directory.join("s"))
            .arg("kill-server")
            .output();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn raw(terminal: &Terminal) -> String {
    String::from_utf8(
        terminal
            .capture_with(CaptureOptions {
                raw: true,
                ..Default::default()
            })
            .unwrap(),
    )
    .unwrap()
}
fn cells(raw: &str) -> BTreeMap<&str, &str> {
    raw.lines()
        .filter_map(|line| {
            line.strip_prefix("\t\tC ")
                .and_then(|line| line.split_once(' '))
        })
        .collect()
}

#[test]
#[ignore = "requires current C reference in TMUXX_C_TMUX"]
fn raw_grid_common_cells_styles_links_markers_and_snapshot_match_c() {
    let bytes = concat!(
        "READY\r\n",
        "\x1b[1;2;3;4;5;7;8;9;53;38;5;123;48;2;1;2;3;58;5;19mA\x1b[0m",
        "\x1b[4:2mB\x1b[4:3mC\x1b[4:4mD\x1b[4:5mE\x1b[0m",
        "\x1b[91;104mF\x1b[0m",
        "\x1b]8;id=token;https://example.com/a?q=1\x1b\\L\x1b]8;;\x1b\\",
        "界e\u{301}\\\r\n",
        "\x1b]133;A\x07>\x1b]133;B\x07command\x1b]133;C\x07output\x1b]133;D;7\x07"
    );
    let reference = Reference::start(bytes.as_bytes(), 60, 6);
    let mut terminal = Terminal::new(60, 6, 2000);
    terminal.feed(bytes.as_bytes());
    let c = reference.raw();
    let native = raw(&terminal);
    assert_eq!(c.lines().next(), native.lines().next());
    let c_cells = cells(&c);
    let native_cells = cells(&native);
    for (row, count) in [(0, 5), (1, 11), (2, 14)] {
        for column in 0..count {
            let position = format!("{row},{column}");
            assert_eq!(
                native_cells[position.as_str()],
                c_cells[position.as_str()],
                "cell {position}"
            );
        }
    }
    let marker = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("\tL 2 "))
            .unwrap()
            .split_once(" osc133=")
            .unwrap()
            .1
            .to_owned()
    };
    assert_eq!(marker(&native), "0,1,8,14,7");
    assert_eq!(marker(&native), marker(&c));
    assert_eq!(raw(&terminal.snapshot().restore().unwrap()), native);
    assert_eq!(
        reference.command(&["capture-pane", "-pRaP", "-S", "999", "-E", "999"]),
        c.as_bytes()
    );
    assert_eq!(
        terminal
            .capture_with(CaptureOptions {
                raw: true,
                alternate: true,
                pending: true,
                start: Some(999),
                end: Some(999),
                ..Default::default()
            })
            .unwrap(),
        native.as_bytes()
    );
}

#[test]
#[ignore = "requires current C reference in TMUXX_C_TMUX"]
fn raw_grid_native_slot_charset_tab_and_clear_differences_are_explicit() {
    let bytes = b"READY\r\n\x1b(0q\x1b(B\r\n\tX\r\n\x1b[38;2;1;2;3mZ\x1b[0m\r\x1b[K";
    let reference = Reference::start(bytes, 20, 6);
    let mut terminal = Terminal::new(20, 6, 2000);
    terminal.feed(bytes);
    let c = reference.raw();
    let native = raw(&terminal);
    assert_ne!(c, native);
    let c_cells = cells(&c);
    let native_cells = cells(&native);
    assert_eq!(
        c_cells["1,0"],
        "data=(1,1,q) flags=NONE[0] attr=CHARSET[80] fg=default[8] bg=default[8] us=default[8] link=NONE linkid=NONE"
    );
    assert_eq!(
        native_cells["1,0"],
        "data=(1,3,─) flags=NONE[0] attr=NONE[0] fg=default[8] bg=default[8] us=default[8] link=NONE linkid=NONE"
    );
    assert_eq!(
        c_cells["2,0"],
        "data=(8,8,        ) flags=TAB[80] attr=NONE[0] fg=default[8] bg=default[8] us=default[8] link=NONE linkid=NONE"
    );
    assert_eq!(
        native_cells["2,0"],
        "data=(1,1, ) flags=CLEARED[40] attr=NONE[0] fg=default[8] bg=default[8] us=default[8] link=NONE linkid=NONE"
    );
    assert_eq!(
        c_cells["3,0"],
        "data=(1,1, ) flags=NONE[0] attr=NONE[0] fg=default[8] bg=default[8] us=default[8] link=NONE linkid=NONE"
    );
    assert_eq!(
        native_cells["3,0"],
        "data=(1,1, ) flags=CLEARED[40] attr=NONE[0] fg=default[8] bg=default[8] us=default[8] link=NONE linkid=NONE"
    );
    assert!(c.lines().any(|line| line == "\tL 4 (4) flags=NONE[0] 0/0"));
    assert!(
        native
            .lines()
            .any(|line| line == "\tL 4 (4) flags=NONE[0] 0/20")
    );
    assert_eq!(raw(&terminal.snapshot().restore().unwrap()), native);
}
