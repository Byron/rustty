//! Kitty protocol observations from a live isolated C server.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
#[cfg(windows)]
use std::cell::Cell;
#[cfg(unix)]
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use std::{
    process::Output,
    time::{Duration, Instant},
};
use tmuxx_terminal::{Effect, Terminal};
#[cfg(unix)]
static NEXT: AtomicU64 = AtomicU64::new(0);
const READER_SCRIPT: &str = r#"import os,select,sys,time,tty
tty.setraw(0)
data=open(sys.argv[1], 'rb').read()+b'\x1b[c'
while data:
    data=data[os.write(1,data):]
reply=b''
deadline=time.monotonic()+10
while time.monotonic()<deadline:
    if select.select([0],[],[],0.05)[0]:
        reply+=os.read(0,65536)
        if b'\x1b[?' in reply and reply.endswith(b'c'):
            with open(sys.argv[2]+'.tmp','wb') as f: f.write(reply.split(b'\x1b[?')[0])
            os.replace(sys.argv[2]+'.tmp',sys.argv[2])
            time.sleep(30)
            break
else:
    raise SystemExit('missing DA barrier')
"#;
struct Reference {
    #[cfg(unix)]
    binary: PathBuf,
    #[cfg(unix)]
    directory: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
    #[cfg(windows)]
    next: Cell<u64>,
}
impl Reference {
    fn new() -> Self {
        #[cfg(unix)]
        let reference = {
            let reference = Self {
                binary: std::env::var_os("TMUXX_C_TMUX")
                    .expect("set TMUXX_C_TMUX")
                    .into(),
                directory: PathBuf::from(format!(
                    "/tmp/tximg-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
            };
            std::fs::create_dir(&reference.directory).unwrap();
            std::fs::write(reference.directory.join("reader.py"), READER_SCRIPT).unwrap();
            reference
        };
        #[cfg(windows)]
        let reference = {
            let remote = remote_reference::RemoteReference::new();
            remote.put(b"reader.py", READER_SCRIPT.as_bytes());
            Self {
                remote,
                next: Cell::new(0),
            }
        };
        let output = reference.command(&["new-session", "-d", "-x", "40", "-y", "12", "sleep 60"]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            reference
                .command(&["display-message", "-p", "#{image_support}"])
                .stdout,
            b"1\n",
            "C reference must have images enabled"
        );
        reference.command(&["set-option", "-g", "status", "off"]);
        reference
    }
    fn command(&self, args: &[&str]) -> Output {
        #[cfg(windows)]
        return self.remote.output(
            &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            &[],
        );
        #[cfg(unix)]
        Command::new(&self.binary)
            .args(["-f", "/dev/null", "-S"])
            .arg(self.directory.join("s"))
            .args(args)
            .env_remove("TMUX")
            .env("TERM", "screen")
            .env("LC_ALL", "C.UTF-8")
            .output()
            .unwrap()
    }
    fn observe(&self, bytes: &[u8]) -> (Vec<u8>, (usize, usize)) {
        #[cfg(unix)]
        let (created, output) = {
            let input = self.directory.join("input");
            let output = self.directory.join("output");
            let _ = std::fs::remove_file(&output);
            std::fs::write(&input, bytes).unwrap();
            let child = format!(
                "/usr/bin/python3 -B {} {} {}",
                self.directory.join("reader.py").display(),
                input.display(),
                output.display()
            );
            (
                self.command(&["new-window", "-d", "-P", "-F", "#{pane_id}", &child]),
                output,
            )
        };
        #[cfg(windows)]
        let (created, output_leaf) = {
            let serial = self.next.get();
            self.next.set(serial + 1);
            let input =
                String::from_utf8(self.remote.put(format!("input-{serial}").as_bytes(), bytes))
                    .unwrap();
            let output_leaf = format!("output-{serial}").into_bytes();
            let output = String::from_utf8(self.remote.path(&output_leaf)).unwrap();
            let reader = String::from_utf8(self.remote.path(b"reader.py")).unwrap();
            (
                self.command(&[
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "python3",
                    "-B",
                    &reader,
                    &input,
                    &output,
                ]),
                output_leaf,
            )
        };
        assert!(created.status.success(), "{created:?}");
        let pane = String::from_utf8(created.stdout).unwrap();
        let pane = pane.trim();
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            #[cfg(unix)]
            let ready = output.exists();
            #[cfg(windows)]
            let ready = self.remote.exists(&output_leaf);
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "missing reference replies");
            std::thread::sleep(Duration::from_millis(5));
        }
        #[cfg(unix)]
        let reply = std::fs::read(output).unwrap();
        #[cfg(windows)]
        let reply = self.remote.get(&output_leaf);
        let cursor = self.command(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{cursor_x},#{cursor_y}",
        ]);
        let cursor = String::from_utf8(cursor.stdout).unwrap();
        let (x, y) = cursor.trim().split_once(',').unwrap();
        assert_eq!(
            self.command(&["display-message", "-p", "-t", pane, "#{pane_title}"])
                .stdout,
            b"kitty-test\n"
        );
        assert!(
            self.command(&["capture-pane", "-p", "-t", pane])
                .stdout
                .iter()
                .all(|b| *b == b'\n')
        );
        self.command(&["kill-pane", "-t", pane]);
        (reply, (x.parse().unwrap(), y.parse().unwrap()))
    }
}
#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        self.command(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn graphics(control: &str, payload: &str) -> String {
    format!("\x1b_G{control};{payload}\x1b\\")
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX with image support"]
fn kitty_upload_query_chunking_and_placement_match_c() {
    let reference = Reference::new();
    let tx = "a=t,i=7,f=32,s=1,v=1";
    let pixel = "/wAA/w==";
    let mut fixtures = vec![
        (
            "png",
            graphics(
                "a=t,i=7,f=100",
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGPgUbLwAwABWQC1RssqBwAAAABJRU5ErkJggg==",
            ),
        ),
        (
            "zlib",
            graphics("a=t,i=7,f=24,s=1,v=1,o=z", "eJyLCnADAAH3APE="),
        ),
        (
            "failed-upload-retains-source",
            graphics(tx, pixel) + &graphics(tx, "!!!!") + &graphics("a=p,i=7,C=1", ""),
        ),
        ("transmit", graphics(tx, pixel)),
        ("defaults", graphics("i=7,s=1,v=1", pixel)),
        ("rgb", graphics("a=t,i=7,f=24,s=1,v=1", "/wAA")),
        ("unpadded", graphics(tx, "/wAA/w")),
        ("placement-id", graphics(&format!("{tx},p=9"), pixel)),
        (
            "chunks",
            graphics(&format!("{tx},m=1"), "/wAA") + &graphics("m=0", "/w=="),
        ),
        (
            "empty-final",
            graphics(&format!("{tx},m=1"), pixel) + &graphics("m=0", ""),
        ),
        ("incomplete", graphics(&format!("{tx},m=1"), "/wAA")),
        ("quiet-one", graphics(&format!("{tx},q=1"), pixel)),
        ("quiet-two", graphics(&format!("{tx},q=2"), pixel)),
        (
            "quiet-chunk",
            graphics(&format!("{tx},q=2,m=1"), "/wAA") + &graphics("m=0", "/w=="),
        ),
        ("anonymous", graphics("a=t,f=32,s=1,v=1", pixel)),
        ("query", graphics("a=q,i=7,f=32,s=1,v=1", pixel)),
        ("query-anonymous", graphics("a=q,f=32,s=1,v=1", pixel)),
        (
            "empty-chunk",
            graphics(&format!("{tx},m=1"), "    ") + &graphics("m=0", pixel),
        ),
        (
            "failed-chunk-recovery",
            graphics(&format!("{tx},m=1"), "/wAA")
                + &graphics("m=0", "!!!!")
                + &graphics(tx, pixel),
        ),
        ("quiet-one-error", graphics(&format!("{tx},q=1"), "!!!!")),
        (
            "query-chunks",
            graphics("a=q,i=7,f=32,s=1,v=1,m=1", "/wAA") + &graphics("m=0", "/w=="),
        ),
        ("replace", graphics(tx, pixel) + &graphics(tx, "AP8A/w==")),
        ("transmit-place", graphics("a=T,i=7,f=32,s=1,v=1", pixel)),
        (
            "transmit-place-sized",
            graphics("a=T,i=7,f=32,s=1,v=1,c=3,r=2", pixel),
        ),
        (
            "stored",
            graphics(&format!("{tx},q=2"), pixel) + &graphics("a=p,i=7", ""),
        ),
        (
            "stationary",
            graphics(&format!("{tx},q=2"), pixel) + &graphics("a=p,i=7,C=1,c=3,r=2", ""),
        ),
        (
            "named",
            graphics(&format!("{tx},q=2"), pixel) + &graphics("a=p,i=7,p=9,C=1", ""),
        ),
        ("missing", graphics("a=p,i=7", "")),
        (
            "query-no-store",
            graphics("a=q,q=2,i=7,f=32,s=1,v=1", pixel) + &graphics("a=p,i=7", ""),
        ),
        ("unknown-control", graphics(&format!("{tx},k=9"), pixel)),
        ("quiet-error", graphics(&format!("{tx},q=2"), "!!!!")),
        ("invalid-payload", graphics(tx, "!!!!")),
        (
            "chunk-invalid-padding",
            graphics(&format!("{tx},m=1"), "/wAA") + &graphics("m=0", "/z=="),
        ),
        ("invalid-format", graphics("a=t,i=7,f=12,s=1,v=1", pixel)),
        ("delete-nonzero-layer", graphics("a=d,d=I,i=7,z=1", "")),
        ("delete-image-number", graphics("a=d,d=I,I=7", "")),
        ("delete-silent-success", graphics("a=d,d=I,i=7", "")),
        ("delete-quiet-error", graphics("a=d,d=z,i=7,q=2", "")),
        (
            "invalid-source-crop",
            graphics(tx, pixel) + &graphics("a=p,i=7,p=9,x=1", ""),
        ),
    ];
    for selector in "cCpPqQrRxXyYzZnNfF".chars() {
        fixtures.push((
            "unsupported-delete-selector",
            graphics(&format!("a=d,d={selector},i=7,p=9"), ""),
        ));
    }
    let mut errors = Vec::new();
    for (name, fixture) in fixtures {
        let bytes = format!("\x1b]2;kitty-test\x07\x1b[3;6H{fixture}").into_bytes();
        let expected = reference.observe(&bytes);
        for chunk_size in [1, 7, bytes.len()] {
            let mut terminal = Terminal::new(40, 12, 2000);
            let reply = bytes
                .chunks(chunk_size)
                .flat_map(|chunk| terminal.feed(chunk))
                .filter_map(|effect| match effect {
                    Effect::Write(bytes) => Some(bytes),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>();
            let actual = (reply, terminal.cursor());
            if actual != expected {
                errors.push(format!(
                    "{name} chunk={chunk_size}: Rust {actual:?}; C {expected:?}"
                ));
            }
            assert_eq!(terminal.title(), b"kitty-test");
            let expected_pixels: Option<&[u8]> = match name {
                "png" => Some(&[12, 34, 56, 78]),
                "zlib" => Some(&[90, 80, 70, 255]),
                "failed-upload-retains-source" => Some(&[255, 0, 0, 255]),
                _ => None,
            };
            if let Some(pixels) = expected_pixels {
                assert_eq!(
                    terminal.snapshot().primary_graphics.images[0]
                        .pixels
                        .as_slice(),
                    pixels
                );
            }
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX with image support"]
fn kitty_image_numbers_and_nonzero_layers_are_explicit_rustty_extensions() {
    let reference = Reference::new();
    for (control, reply_control) in [
        ("a=t,I=7,f=32,s=1,v=1", "I=7"),
        ("a=t,i=7,f=32,s=1,v=1,z=1", "i=7"),
    ] {
        let bytes = format!(
            "\x1b]2;kitty-test\x07\x1b[3;6H{}",
            graphics(control, "/wAA/w==")
        )
        .into_bytes();
        let c = reference.observe(&bytes);
        assert_eq!(c, (graphics(reply_control, "ENOTSUP").into_bytes(), (5, 2)));
        let mut terminal = Terminal::new(40, 12, 2000);
        let replies = terminal
            .feed(&bytes)
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Write(bytes) => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert!(replies.windows(3).any(|bytes| bytes == b";OK"));
        assert_eq!(terminal.snapshot().primary_graphics.images.len(), 1);
    }
}

#[test]
fn tmux_graphics_profile_and_pending_query_survive_snapshot_and_reset() {
    let mut terminal = Terminal::new(40, 12, 2000);
    assert!(
        terminal
            .feed(graphics("a=q,i=7,f=32,s=1,v=1,m=1", "/wAA").as_bytes())
            .is_empty()
    );
    let mut restored = terminal.snapshot().restore().unwrap();
    let replies = restored.feed(graphics("m=0", "/w==").as_bytes());
    assert!(
        matches!(replies.as_slice(),[Effect::Write(bytes)] if bytes == graphics("i=7","OK").as_bytes())
    );
    assert!(restored.snapshot().primary_graphics.images.is_empty());
    restored.feed(b"\x1bc");
    let replies = restored.feed(graphics("a=p,i=7", "").as_bytes());
    assert!(
        matches!(replies.as_slice(),[Effect::Write(bytes)] if bytes == graphics("i=7","ENOENT").as_bytes())
    );
}

#[test]
#[ignore = "requires isolated Unix sockets and TMUXX_C_TMUX with image support"]
fn per_image_encoded_accumulated_decoded_and_placement_limits_match_c() {
    let reference = Reference::new();
    let tx = "a=t,i=7,f=32,s=1,v=1";
    for (name, limit, fixture) in [
        ("encoded-too-large", 7, graphics(tx, "/wAA/w==")),
        ("encoded-boundary", 8, graphics(tx, "/wAA/w==")),
        (
            "chunked-decoded-boundary",
            4,
            graphics(&format!("{tx},m=1"), "/wAA") + &graphics("m=0", "/w=="),
        ),
        (
            "chunked-too-large",
            3,
            graphics(&format!("{tx},m=1"), "/wAA") + &graphics("m=0", "/w=="),
        ),
        (
            "placement-area",
            8,
            graphics(tx, "/wAA/w==") + &graphics("a=p,i=7,C=1,c=3,r=3", ""),
        ),
        (
            "rgba-decoded-too-large",
            15,
            graphics("a=t,i=7,f=32,s=2,v=2,m=1", "/wAA//8AAP8=") + &graphics("m=0", "/wAA//8AAP8="),
        ),
        (
            "rgba-decoded-boundary",
            16,
            graphics("a=t,i=7,f=32,s=2,v=2,m=1", "/wAA//8AAP8=") + &graphics("m=0", "/wAA//8AAP8="),
        ),
    ] {
        assert!(
            reference
                .command(&["set-option", "-s", "image-size-limit", &limit.to_string()])
                .status
                .success()
        );
        let bytes = format!("\x1b]2;kitty-test\x07\x1b[3;6H{fixture}").into_bytes();
        let expected = reference.observe(&bytes);
        let mut terminal = Terminal::new(40, 12, 2000);
        terminal.set_image_size_limit(limit);
        let replies = terminal
            .feed(&bytes)
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Write(bytes) => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!((replies, terminal.cursor()), expected, "{name}");
    }
}
