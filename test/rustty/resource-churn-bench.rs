//! Repaint a bounded live row with new page resources; setup is untimed.
use criterion::{Criterion, Throughput};
use rustty_vt::{Color, ScrollbackLimits, Style, Terminal};
use std::{fmt::Write, hint::black_box};

const CELLS: usize = 80;
const FRAMES: usize = 16;

fn color(value: usize) -> Color {
    Color::Rgb(value as u8, (value >> 8) as u8, 1)
}

fn input(kind: &str) -> Vec<u8> {
    let mut input = String::new();
    for frame in 0..FRAMES {
        input.push_str("\x1b[H");
        for col in 0..CELLS {
            let value = frame * CELLS + col;
            match kind {
                "style" => {
                    let Color::Rgb(r, g, b) = color(value) else {
                        unreachable!()
                    };
                    write!(input, "\x1b[38;2;{r};{g};{b}mX").unwrap();
                }
                "hyperlink" => write!(
                    input,
                    "\x1b]8;id={value};https://example.org/{value}\x07X\x1b]8;;\x07"
                )
                .unwrap(),
                "plain" => input.push('X'),
                _ => unreachable!(),
            }
        }
        input.push_str("\x1b[0m");
    }
    input.into_bytes()
}

fn check(terminal: &Terminal, kind: &str) {
    let screen = terminal.screen();
    assert_eq!((screen.cursor.row, screen.cursor.col), (0, CELLS));
    assert_eq!(screen.cursor.style, Style::default());
    assert_eq!(screen.history_len(), 0);
    let row = screen.row(0);
    for (col, cell) in row.cells.iter().enumerate() {
        assert_eq!(cell.codepoint(), (col < CELLS).then_some('X'));
        let value = (FRAMES - 1) * CELLS + col;
        assert_eq!(
            row.style(col).foreground,
            if kind == "style" && col < CELLS {
                color(value)
            } else {
                Color::Default
            }
        );
        let uri =
            (kind == "hyperlink" && col < CELLS).then(|| format!("https://example.org/{value}"));
        assert_eq!(
            row.hyperlink(col).map(|link| link.uri_bytes()),
            uri.as_deref().map(str::as_bytes)
        );
    }
}

pub fn resources(c: &mut Criterion) {
    let mut group = c.benchmark_group("rustty/resource_churn");
    for kind in ["plain", "style", "hyperlink"] {
        let input = input(kind);
        let mut terminal = Terminal::with_limits(120, 2, ScrollbackLimits::NONE);
        for _ in 0..4 {
            assert!(terminal.feed(&input).is_empty());
        }
        check(&terminal, kind);
        group.throughput(Throughput::Elements((CELLS * FRAMES) as u64));
        group.bench_function(kind, |b| {
            b.iter(|| black_box(black_box(&mut terminal).feed(black_box(&input))))
        });
        check(&terminal, kind);
    }
    group.finish();
}
