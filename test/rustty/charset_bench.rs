//! Whole-screen legacy-charset repaint and steady-state scroll workloads.
use criterion::{Criterion, Throughput};
use rustty_vt::{Color, ScrollbackLimits, Style, Terminal};
use std::hint::black_box;

const COLS: u16 = 128;
const ROWS: u16 = 32;
const HISTORY: usize = 1024;
const CHARSETS: [(&str, char, &str, &str); 3] = [
    ("ascii", 'B', "abc#defq", "abc#defq"),
    ("dec", '0', "lqqkxmjq", "┌──┐│└┘─"),
    ("british", 'A', "#pounds#", "£pounds£"),
];

fn input(set: char, pattern: &str, stream: bool) -> Vec<u8> {
    let mut input = format!("\x1b({set}");
    for row in 0..ROWS {
        if !stream {
            input.push_str(&format!("\x1b[{};1H", row + 1));
        }
        input.push_str(&format!(
            "\x1b[{};{}m",
            if row % 2 == 0 { 1 } else { 22 },
            31 + row % 4,
        ));
        input.push_str(&pattern.repeat(if stream { 24 } else { 16 }));
        if stream {
            input.push_str("\r\n");
        }
    }
    input.push_str("\x1b[0m");
    input.into_bytes()
}

fn check(terminal: &Terminal, pattern: &str, mapped: &str, stream: bool) -> u64 {
    let screen = terminal.screen();
    assert_eq!(screen.cursor.row, usize::from(ROWS) - 1);
    assert_eq!(screen.cursor.col, if stream { 0 } else { 127 });
    assert_eq!(screen.cursor.pending_wrap, !stream);
    assert_eq!(screen.cursor.style, Style::default());
    assert_eq!(screen.history_len() == 0, !stream);
    assert!(screen.history_len() <= HISTORY);
    let mapped: Vec<_> = mapped.chars().collect();
    let mut checksum = 0u64;
    for (index, row) in screen.rows().enumerate() {
        let used = if !stream {
            128
        } else if index == 31 {
            0
        } else if index % 2 == 0 {
            64
        } else {
            128
        };
        let record = if stream {
            16 + index.div_ceil(2)
        } else {
            index
        };
        for (col, cell) in row.cells.iter().enumerate() {
            let expected = (col < used).then(|| mapped[col % mapped.len()]);
            assert_eq!(cell.codepoint(), expected);
            assert_eq!(cell.width(), 1);
            assert!(!cell.has_grapheme());
            if expected.is_some() {
                assert_eq!(
                    row.style(col),
                    Style {
                        foreground: Color::Indexed(1 + (record % 4) as u8),
                        bold: record % 2 == 0,
                        ..Style::default()
                    }
                );
            }
            checksum = checksum
                .wrapping_mul(16_777_619)
                .wrapping_add(cell.codepoint().map_or(0, u64::from));
        }
    }
    // REP must remember the input byte, even after the charset changes.
    let mut repeat = terminal.clone();
    assert!(repeat.feed(b"\x1b(B\x1b[H\x1b[b").is_empty());
    assert_eq!(
        repeat.screen().row(0).cells[0].codepoint(),
        pattern.chars().last()
    );
    checksum
}

pub fn charsets(c: &mut Criterion) {
    for stream in [false, true] {
        let mut group = c.benchmark_group(if stream {
            "rustty/charset_stream"
        } else {
            "rustty/charset_repaint"
        });
        for (name, set, pattern, mapped) in CHARSETS {
            let input = input(set, pattern, stream);
            let mut terminal = Terminal::with_limits(
                COLS,
                ROWS,
                if stream {
                    ScrollbackLimits {
                        bytes: None,
                        lines: Some(HISTORY),
                    }
                } else {
                    ScrollbackLimits::NONE
                },
            );
            // Reach bounded history eviction before measuring scroll throughput.
            for _ in 0..if stream { 32 } else { 2 } {
                assert!(terminal.feed(&input).is_empty());
            }
            let checksum = check(&terminal, pattern, mapped, stream);
            group.throughput(Throughput::Bytes(input.len() as u64));
            group.bench_function(name, |b| {
                b.iter(|| black_box(black_box(&mut terminal).feed(black_box(&input))))
            });
            assert_eq!(check(&terminal, pattern, mapped, stream), checksum);
        }
        group.finish();
    }
}
