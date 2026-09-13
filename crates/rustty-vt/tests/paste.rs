use base64::Engine;
use rustty_vt::{Effect, Terminal, clipboard, paste};
use std::io;

fn zero_entropy(buffer: &mut [u8]) -> io::Result<()> {
    buffer.fill(0);
    Ok(())
}

fn request_grants(terminal: &mut Terminal, mimes: &[&[u8]]) -> Vec<bool> {
    let mut grants = Vec::new();
    for mime in mimes {
        let bytes = format!(
            "\x1b]5522;type=read:name=cHJvZ3JhbQ==:pw={};{}\x07",
            base64::prelude::BASE64_STANDARD.encode(b"2222222222222222222222"),
            base64::prelude::BASE64_STANDARD.encode(mime),
        );
        for effect in terminal.feed(bytes.as_bytes()) {
            if let Effect::ClipboardRead(read) = effect {
                grants.push(read.granted);
            }
        }
    }
    grants
}

#[test]
fn kitty_paste_lists_without_reading_and_grants_one_data_request() {
    let mut terminal = Terminal::new(20, 2, 0);
    terminal.feed(b"\x1b[?5522;2004h");
    let mimes = vec![b"image/png".to_vec(), b"text/plain".to_vec()];
    let mut output = Vec::new();
    let pasted = terminal
        .paste(
            paste::Request {
                source: paste::Source::Clipboard(clipboard::Location::Selection),
                contents: paste::Contents::Reader {
                    mimes: &mimes,
                    read: &mut |_| panic!("paste event must not read clipboard contents"),
                },
                allow_unsafe: false,
            },
            Some(&mut zero_entropy),
            &mut output,
        )
        .unwrap();
    assert!(pasted);
    assert!(output.starts_with(b"\x1b]5522;type=read:status=OK:loc=primary:pw="));
    assert_eq!(output.windows(4).filter(|part| *part == b":pw=").count(), 3);
    assert_eq!(
        request_grants(&mut terminal, &[b".", b"text/plain", b"text/plain"]),
        [false, true, false]
    );
}

#[test]
fn failed_event_write_revokes_its_grant_and_entropy_failure_writes_nothing() {
    let mut terminal = Terminal::new(20, 2, 0);
    terminal.feed(b"\x1b[?5522h");
    let request = || paste::Request {
        source: paste::Source::Clipboard(clipboard::Location::Standard),
        contents: paste::Contents::Memory(&[]),
        allow_unsafe: false,
    };
    let mut too_small = [0; 8];
    assert!(matches!(
        terminal.paste(request(), Some(&mut zero_entropy), &mut &mut too_small[..]),
        Err(paste::Error::WriteFailed(_))
    ));
    assert_eq!(request_grants(&mut terminal, &[b"text/plain"]), [false]);
    let mut output = Vec::new();
    assert!(matches!(
        terminal.paste(
            request(),
            Some(&mut |_| Err(io::Error::other("no entropy"))),
            &mut output
        ),
        Err(paste::Error::EntropyUnavailable(_))
    ));
    assert!(output.is_empty());
    assert_eq!(request_grants(&mut terminal, &[b"text/plain"]), [false]);
}

#[test]
fn ordinary_paste_reads_once_and_refuses_unsafe_or_failed_reads_before_writing() {
    let mut terminal = Terminal::new(20, 2, 0);
    terminal.feed(b"\x1b[?5522h");
    let mimes = vec![
        b"image/png".to_vec(),
        b"TEXT".to_vec(),
        b"text/plain".to_vec(),
    ];
    for fail in [false, true] {
        let mut calls = 0;
        let mut read = |mime: &[u8]| {
            calls += 1;
            assert_eq!(mime, b"TEXT");
            if fail {
                Err(io::Error::other("unavailable"))
            } else {
                Ok(b"first\nsecond".to_vec())
            }
        };
        let mut output = Vec::new();
        let result = terminal.paste(
            paste::Request {
                source: paste::Source::Clipboard(clipboard::Location::Standard),
                contents: paste::Contents::Reader {
                    mimes: &mimes,
                    read: &mut read,
                },
                allow_unsafe: false,
            },
            None,
            &mut output,
        );
        assert!(if fail {
            matches!(result, Err(paste::Error::ReadFailed(_)))
        } else {
            matches!(result, Err(paste::Error::UnsafePaste))
        });
        assert!(output.is_empty());
        assert_eq!(calls, 1);
    }
    terminal.feed(b"\x1b[?2004h");
    let mut output = Vec::new();
    assert!(
        terminal
            .paste(
                paste::Request {
                    source: paste::Source::Text,
                    contents: paste::Contents::Reader {
                        mimes: &mimes,
                        read: &mut |_| Ok(b"first\nsecond".to_vec())
                    },
                    allow_unsafe: false,
                },
                Some(&mut |_| panic!("text insertion needs no entropy")),
                &mut output
            )
            .unwrap()
    );
    assert_eq!(output, b"\x1b[200~first\nsecond\x1b[201~");
}
