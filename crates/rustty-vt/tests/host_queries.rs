use rustty_vt::query::{ColorScheme, DeviceAttributes, Size, SizeStyle};
use rustty_vt::{Effect, EffectHandler, Terminal};

#[derive(Default)]
struct Readonly(Vec<Effect>);
impl EffectHandler for Readonly {
    fn effect(&mut self, effect: Effect) {
        self.0.push(effect);
    }
}

#[test]
fn linefeed_mode_effects_keep_stream_order_including_reset() {
    let stream = b"\x1b[20h\x1b[5n\x1b[20l\x1b[5n\x1bc";
    for split in 0..=stream.len() {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.linefeed_mode_events = true;
        let mut effects = terminal.feed(&stream[..split]);
        effects.extend(terminal.feed(&stream[split..]));
        assert_eq!(
            effects,
            [
                Effect::LinefeedMode(true),
                Effect::Write(b"\x1b[0n".to_vec()),
                Effect::LinefeedMode(false),
                Effect::Write(b"\x1b[0n".to_vec()),
                Effect::LinefeedMode(false),
                Effect::Progress {
                    state: 0,
                    value: None
                },
            ]
        );
        assert!(terminal.linefeed_mode_events);
    }
}

#[test]
fn focus_reporting_sends_current_host_state_on_enable_and_restore() {
    for focused in [false, true] {
        let mut terminal = Terminal::new(10, 2, 0);
        assert!(terminal.feed(b"\x1b[?1004h").is_empty());
        terminal.query_defaults.focused = Some(focused);
        let report = Effect::Write(if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec());
        for sequence in [
            b"\x1b[?1004h".as_slice(),
            b"\x1b[?1004s\x1b[?1004l\x1b[?1004r",
        ] {
            assert_eq!(
                terminal.feed(sequence).as_slice(),
                std::slice::from_ref(&report)
            );
        }
        terminal.feed(b"\x1bc");
        assert_eq!(terminal.feed(b"\x1b[?1004h"), [report]);
    }
}

#[test]
fn synchronous_hosts_are_readonly_unless_callbacks_supply_answers() {
    let mut terminal = Terminal::new(10, 2, 0);
    let mut host = Readonly::default();
    terminal.feed_with_handler(
        b"\x05\x1bZ\x1b[c\x1b[>c\x1b[=c\x1b[?996n\x1b[14t\x1b[16t\x1b[18t\x1b[>q",
        &mut host,
    );
    assert_eq!(host.0, [Effect::Write(b"\x1bP>|libghostty\x1b\\".to_vec())]);
}

#[test]
fn host_queries_use_binary_answers_and_preserve_callback_order() {
    #[derive(Default)]
    struct Host {
        calls: Vec<&'static str>,
        writes: Vec<Vec<u8>>,
    }
    impl EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            if let Effect::Write(bytes) = effect {
                self.calls.push("write");
                self.writes.push(bytes);
            }
        }
        fn enquiry(&mut self) -> Vec<u8> {
            self.calls.push("enquiry");
            vec![0, 255]
        }
        fn xtversion(&mut self) -> Vec<u8> {
            self.calls.push("version");
            b"custom\xff".to_vec()
        }
        fn color_scheme(&mut self) -> Option<ColorScheme> {
            self.calls.push("scheme");
            Some(ColorScheme::Light)
        }
        fn device_attributes(&mut self) -> Option<DeviceAttributes> {
            self.calls.push("attributes");
            Some(DeviceAttributes {
                conformance_level: 65,
                features: vec![22, 52],
                device_type: 64,
                firmware_version: 123,
                rom_cartridge: 2,
                unit_id: 0xabcdef01,
            })
        }
        fn size(&mut self) -> Option<Size> {
            self.calls.push("size");
            Some(Size {
                rows: 12,
                columns: 34,
                cell_width: 9,
                cell_height: 18,
            })
        }
    }
    let mut terminal = Terminal::new(10, 2, 0);
    let mut host = Host::default();
    terminal.feed_with_handler(
        b"\x05\x1b[>q\x1b[?996n\x1b[c\x1b[>c\x1b[=c\x1b[14t\x1b[?2048h",
        &mut host,
    );
    assert_eq!(
        host.calls,
        [
            "enquiry",
            "write",
            "version",
            "write",
            "scheme",
            "write",
            "attributes",
            "write",
            "attributes",
            "write",
            "attributes",
            "write",
            "size",
            "write",
            "size",
            "write"
        ]
    );
    assert_eq!(
        host.writes,
        [
            vec![0, 255],
            b"\x1bP>|custom\xff\x1b\\".to_vec(),
            b"\x1b[?997;2n".to_vec(),
            b"\x1b[?65;22;52c".to_vec(),
            b"\x1b[>64;123;2c".to_vec(),
            b"\x1bP!|ABCDEF01\x1b\\".to_vec(),
            b"\x1b[4;216;306t".to_vec(),
            b"\x1b[48;12;34;216;306t".to_vec(),
        ]
    );
}

#[test]
fn host_defaults_survive_ris_and_are_reapplied_after_snapshot_restore() {
    let mut terminal = Terminal::new(10, 2, 0);
    assert_eq!(
        terminal.feed(b"\x1b[>0q"),
        [Effect::Write(
            concat!("\x1bP>|ghostty ", env!("CARGO_PKG_VERSION"), "\x1b\\")
                .as_bytes()
                .to_vec()
        )]
    );
    terminal.query_defaults.xtversion = b"embedder".to_vec();
    terminal.query_defaults.enquiry = b"answer".to_vec();
    terminal.query_defaults.color_scheme = Some(ColorScheme::Dark);
    terminal.query_defaults.device_attributes = None;
    terminal.query_defaults.size = Some(Size {
        columns: 20,
        rows: 3,
        cell_width: 9,
        cell_height: 18,
    });
    terminal.title_report = true;
    terminal.visible = false;
    terminal.terminfo_name = Some(vec![b'r', 255]);
    terminal.feed(b"\x1bc\x1b]2;title\x07");
    assert_eq!(
        terminal.feed(b"\x05\x1b[c\x1b[>q\x1b[?996n\x1b[?998n\x1b[21t\x1b[18t"),
        [
            Effect::Write(b"answer".to_vec()),
            Effect::Write(b"\x1bP>|embedder\x1b\\".to_vec()),
            Effect::Write(b"\x1b[?997;1n".to_vec()),
            Effect::Write(b"\x1b[?999;2n".to_vec()),
            Effect::Write(b"\x1b]ltitle\x1b\\".to_vec()),
            Effect::Write(b"\x1b[8;3;20t".to_vec()),
        ]
    );
    let bytes = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let restored = rustty_vt::snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
    assert_ne!(restored.query_defaults, terminal.query_defaults);
    assert_eq!(restored.query_defaults.size, None);
    assert!(!restored.title_report);
    assert!(restored.visible);
    assert_eq!(restored.terminfo_name, None);
    assert_eq!(terminal.terminfo_name, Some(vec![b'r', 255]));
}

#[test]
fn resize_reports_complete_geometry_without_truncating_wire_values() {
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.modes.set(true, 2048, true);
    assert!(terminal.resize_with_cell_size(10, 2, None).is_empty());
    assert_eq!(
        terminal.resize_with_cell_size(10, 2, Some((9, 18))),
        [Effect::Write(b"\x1b[48;2;10;36;90t".to_vec())]
    );
    terminal.modes.set(true, 2026, true);
    let report = terminal.resize_with_cell_size(10, 2, Some((u32::MAX, u32::MAX)));
    assert_eq!(
        report,
        [Effect::Write(
            b"\x1b[48;2;10;8589934590;42949672950t".to_vec()
        )]
    );
    assert_eq!(
        (terminal.width_px, terminal.height_px),
        (u32::MAX, u32::MAX)
    );
    assert!(!terminal.modes.dec(2026));
    assert_eq!(
        Size {
            rows: u16::MAX,
            columns: u16::MAX,
            cell_width: u32::MAX,
            cell_height: u32::MAX
        }
        .encode(SizeStyle::TextPixels),
        b"\x1b[4;281470681677825;281470681677825t"
    );
}
