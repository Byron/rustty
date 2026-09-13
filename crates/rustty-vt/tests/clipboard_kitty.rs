use base64::{Engine, engine::general_purpose::STANDARD};
use rustty_vt::clipboard::{Content, Read, ReadResult, ReadSuccess, Write, WriteResult};
use rustty_vt::{Effect, EffectHandler, Terminal};
use std::sync::Arc;

fn packet(metadata: &str, payload: &str) -> Vec<u8> {
    format!("\x1b]5522;{metadata};{payload}\x1b\\").into_bytes()
}

#[derive(Default)]
struct Host {
    events: Vec<Effect>,
    remember: bool,
}

impl EffectHandler for Host {
    fn effect(&mut self, effect: Effect) {
        self.events.push(effect);
    }

    fn clipboard_read(&mut self, read: &Read) -> ReadResult {
        self.events.push(Effect::ClipboardRead(read.clone()));
        ReadResult::Success(ReadSuccess {
            contents: vec![Content {
                mime: b"text/plain".to_vec(),
                data: b"hello".as_slice().into(),
            }],
            available: vec![b"text/plain".to_vec()],
            remember: self.remember,
        })
    }

    fn clipboard_write(&mut self, write: &Write) -> WriteResult {
        self.events.push(Effect::ClipboardWrite(write.clone()));
        WriteResult::Success {
            remember: self.remember,
        }
    }
}

#[test]
fn multipart_clipboard_writes_share_aliases_and_keep_the_opening_identity() {
    let mut terminal = Terminal::new(10, 2, 0);
    let mut host = Host::default();
    terminal.feed_with_handler(&packet("type=write:id=first", ""), &mut host);
    let metadata = format!("type=wdata:mime={}", STANDARD.encode("text/plain"));
    for chunk in ["a", "GVs", "bG8", "="] {
        terminal.feed_with_handler(&packet(&metadata, chunk), &mut host);
    }
    terminal.feed_with_handler(
        &packet(
            &format!("type=walias:mime={}", STANDARD.encode("text/plain")),
            &STANDARD.encode("TEXT\u{b}UTF8_STRING"),
        ),
        &mut host,
    );
    terminal.feed_with_handler(b"\x1b]5522;type=wdata:id=ignored\x07", &mut host);
    let Effect::ClipboardWrite(write) = &host.events[0] else {
        panic!("missing write")
    };
    assert_eq!(write.contents.len(), 3);
    assert_eq!(&*write.contents[0].data, b"hello");
    assert!(Arc::ptr_eq(
        &write.contents[0].data,
        &write.contents[1].data
    ));
    assert!(Arc::ptr_eq(
        &write.contents[0].data,
        &write.contents[2].data
    ));
    assert_eq!(
        host.events[1],
        Effect::Write(b"\x1b]5522;type=write:status=DONE:id=first\x07".to_vec())
    );
}

#[test]
fn transfer_limit_is_captured_and_invalid_chunks_abort_without_partial_writes() {
    let mut terminal = Terminal::new(10, 2, 0);
    let mut host = Host::default();
    let metadata = format!("type=wdata:mime={}", STANDARD.encode("text/plain"));
    terminal.clipboard_write_limit = 2;
    terminal.feed_with_handler(&packet("type=write", ""), &mut host);
    terminal.clipboard_write_limit = 100;
    terminal.feed_with_handler(&packet(&metadata, "YWJj"), &mut host);
    terminal.feed_with_handler(&packet("type=wdata", ""), &mut host);
    assert_eq!(
        host.events,
        [Effect::Write(
            b"\x1b]5522;type=write:status=EFBIG\x1b\\".to_vec()
        )]
    );

    for data in ["Zg", "Zg=", "Z?", "Zg==Zg=="] {
        host.events.clear();
        terminal.feed_with_handler(&packet("type=write", ""), &mut host);
        terminal.feed_with_handler(&packet(&metadata, data), &mut host);
        terminal.feed_with_handler(&packet("type=wdata", ""), &mut host);
        assert_eq!(
            host.events,
            [Effect::Write(
                b"\x1b]5522;type=write:status=EINVAL\x1b\\".to_vec()
            )],
            "{data}"
        );
    }
}

#[test]
fn grants_are_applied_before_next_request_and_ris_preserves_the_open_transfer() {
    let mut terminal = Terminal::new(10, 2, 0);
    let mut host = Host {
        remember: true,
        ..Host::default()
    };
    let metadata = format!(
        "type=read:name={}:pw={}",
        STANDARD.encode("program"),
        STANDARD.encode("password")
    );
    let read = packet(&metadata, &STANDARD.encode("text/plain"));
    let mut input = read.clone();
    input.extend(&read);
    terminal.feed_with_handler(&input, &mut host);
    let grants: Vec<_> = host
        .events
        .iter()
        .filter_map(|e| match e {
            Effect::ClipboardRead(read) => Some(read.granted),
            _ => None,
        })
        .collect();
    assert_eq!(grants, [false, true]);

    terminal.feed_with_handler(&packet("type=write:id=ongoing", ""), &mut host);
    let data = packet(
        &format!("type=wdata:mime={}", STANDARD.encode("text/plain")),
        "Zg==",
    );
    terminal.feed_with_handler(&data, &mut host);
    terminal.feed_with_handler(b"\x1bc", &mut host);
    host.events.clear();
    terminal.feed_with_handler(&read, &mut host);
    let Effect::ClipboardRead(read) = &host.events[0] else {
        panic!("missing read")
    };
    assert!(!read.granted);
    host.events.clear();
    terminal.feed_with_handler(&packet("type=wdata", ""), &mut host);
    let Effect::ClipboardWrite(write) = &host.events[0] else {
        panic!("missing write")
    };
    assert_eq!(&*write.contents[0].data, b"f");
    assert_eq!(
        host.events[1],
        Effect::Write(b"\x1b]5522;type=write:status=DONE:id=ongoing\x1b\\".to_vec())
    );
}
