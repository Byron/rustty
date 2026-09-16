//! Supplemental output checks for an active screen spanning several pages.
use criterion::{Criterion, criterion_group, criterion_main};
use rustty_vt::{ScrollbackLimits, Terminal};
use std::hint::black_box;

fn page_spans(c: &mut Criterion) {
    let mut terminal = Terminal::with_limits(
        1024,
        96,
        ScrollbackLimits {
            bytes: None,
            lines: Some(1024),
        },
    );
    let record = format!("{}\r\n", "abcdefgh".repeat(192));
    let plain = record.repeat(8);
    let styled = format!("\x1b[31m{record}\x1b[0m{record}").repeat(4);
    for _ in 0..128 {
        assert!(terminal.feed(plain.as_bytes()).is_empty());
    }
    let mut group = c.benchmark_group("rustty/page_spans");
    group.bench_function("print", |b| {
        b.iter(|| {
            let terminal = black_box(&mut terminal);
            for row in [0, 48, 95] {
                let cursor = &mut terminal.screen_mut().cursor;
                cursor.row = row;
                cursor.col = 0;
                cursor.pending_wrap = false;
                for byte in black_box(record.as_bytes()).iter().take(1024) {
                    terminal.print(char::from(*byte));
                }
                assert!(terminal.screen().cursor.pending_wrap);
            }
        });
    });
    for (name, input) in [("stream", plain), ("styled", styled)] {
        terminal.feed(b"\x1b[96;1H");
        group.bench_function(name, |b| {
            b.iter(|| black_box(terminal.feed(black_box(input.as_bytes()))));
        });
        assert_eq!(terminal.screen().cursor.row, 95);
        assert_eq!(terminal.screen().cursor.col, 0);
        assert!(terminal.screen().history_len() <= 1024);
    }
    group.finish();
}

criterion_group!(benches, page_spans);
criterion_main!(benches);
