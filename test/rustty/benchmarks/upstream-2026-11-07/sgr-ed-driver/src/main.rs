//! Scratch throughput driver: input/setup/checks are outside each timed batch.
//! Controlled SGR/erase fixtures complement the upstream generated-file corpus.
use rustty_vt::{Color, ScrollbackLimits, Style, Terminal};
use std::{fmt::Write as _, hint::black_box, path::Path, time::Instant};

const COLS: u16 = 120;
const FRAMES: usize = 64;
const ROUNDS: usize = 3;

struct Workload {
    bytes: Vec<u8>,
    cells: Vec<Option<char>>,
    styles: Vec<Style>,
    cursor: (usize, usize),
    allows_history: bool,
}

fn workload(case: &str, rows: u16) -> Workload {
    assert!(rows >= 3);
    let mut input = String::from("\x1b[0m");
    let mut cells = vec![None; usize::from(COLS) * usize::from(rows)];
    let mut styles = vec![Style::default(); cells.len()];
    let mut cursor = (0, 0);
    if case.starts_with("sgr-") || case == "plain" {
        let attrs = if case == "plain" {
            0
        } else {
            case[4..].parse::<usize>().unwrap()
        };
        assert!(attrs <= 4);
        for frame in 0..FRAMES {
            let row = frame % usize::from(rows);
            write!(input, "\x1b[{};1H", row + 1).unwrap();
            for run in 0..20 {
                let mut style = Style::default();
                if attrs > 0 {
                    let r = ((frame + run) % 6 * 51) as u8;
                    let g = ((frame / 6 + run) % 6 * 51) as u8;
                    let b = ((frame + run / 6) % 6 * 51) as u8;
                    style.foreground = Color::Rgb(r, g, b);
                    write!(input, "\x1b[38;2;{r};{g};{b}").unwrap();
                    if attrs >= 2 {
                        style.bold = (frame + run) % 2 == 0;
                        write!(input, ";{}", if style.bold { 1 } else { 22 }).unwrap();
                    }
                    if attrs >= 3 {
                        let bg = ((frame * 3 + run) % 16) as u8;
                        style.background = Color::Indexed(bg);
                        write!(input, ";48;5;{bg}").unwrap();
                    }
                    if attrs >= 4 {
                        style.italic = (frame + run) % 3 == 0;
                        write!(input, ";{}", if style.italic { 3 } else { 23 }).unwrap();
                    }
                    input.push('m');
                }
                input.push_str("Abcd");
                for (offset, cp) in "Abcd".chars().enumerate() {
                    let index = row * usize::from(COLS) + run * 4 + offset;
                    cells[index] = Some(cp);
                    styles[index] = style;
                }
            }
            input.push_str("\x1b[0m");
            cursor = (row, 80);
        }
    } else {
        let (mode, extra_lines) = match case {
            "erase-2" | "erase-2-seen" => (2, 0),
            "erase-22" => (22, 0),
            "erase-lines" => (2, 40),
            _ => panic!("unknown case {case}"),
        };
        for frame in 0..FRAMES {
            write!(input, "\x1b[{mode}J").unwrap();
            for clear in 0..extra_lines {
                let row = (frame * 17 + clear * 7) % usize::from(rows) + 1;
                if clear % 2 == 0 {
                    write!(input, "\x1b[{row};1H\x1b[2K").unwrap();
                } else {
                    let col = (frame * 13 + clear * 11) % usize::from(COLS) + 1;
                    write!(input, "\x1b[{row};{col}H\x1b[K").unwrap();
                }
            }
            let row = 1 + (frame * 17) % (usize::from(rows) - 2);
            let col = 1 + (frame * 13) % (usize::from(COLS) - 6);
            write!(
                input,
                "\x1b[Hheader\x1b[{};{}Hnoise\x1b[2K\x1b[{};1H\x1b[31mredraw\x1b[0m\r\n",
                row + 1,
                col,
                row + 1
            )
            .unwrap();
            cells.fill(None);
            styles.fill(Style::default());
            for (offset, cp) in "header".chars().enumerate() {
                cells[offset] = Some(cp);
            }
            for (offset, cp) in "redraw".chars().enumerate() {
                let index = row * usize::from(COLS) + offset;
                cells[index] = Some(cp);
                styles[index].foreground = Color::Indexed(1);
            }
            cursor = (row + 1, 0);
        }
    }
    Workload {
        bytes: input.into_bytes(),
        cells,
        styles,
        cursor,
        allows_history: matches!(case, "erase-22" | "erase-2-seen"),
    }
}

fn check(terminal: &Terminal, expected: &Workload) {
    let screen = terminal.screen();
    if expected.allows_history {
        // Native limits retain an active page floor even when zero is requested.
        let page_rows = screen
            .page_allocations()
            .map(|p| usize::from(p.capacity.rows))
            .max()
            .unwrap();
        assert!(screen.history_len() <= page_rows);
    } else {
        assert_eq!(screen.history_len(), 0);
    }
    assert_eq!((screen.cursor.row, screen.cursor.col), expected.cursor);
    assert!(!screen.cursor.pending_wrap);
    assert_eq!(screen.cursor.style, Style::default());
    for (row_index, row) in screen.rows().enumerate() {
        for (col, cell) in row.cells.iter().enumerate() {
            let index = row_index * usize::from(COLS) + col;
            assert_eq!(
                cell.codepoint(),
                expected.cells[index],
                "cell {row_index}:{col}"
            );
            assert_eq!(
                row.style(col),
                expected.styles[index],
                "style {row_index}:{col}"
            );
        }
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        (2..=4).contains(&args.len()),
        "usage: driver CASE [ITERATIONS=100] [ROWS=80]"
    );
    let case = &args[1];
    let iterations = args.get(2).map_or(100, |s| s.parse::<usize>().unwrap());
    let rows = args.get(3).map_or(80, |s| s.parse::<u16>().unwrap());
    let expected = workload(case, rows);
    let corpus_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpora");
    std::fs::create_dir_all(&corpus_dir).unwrap();
    std::fs::write(
        corpus_dir.join(format!("{case}-{COLS}x{rows}.vt")),
        &expected.bytes,
    )
    .unwrap();
    let mut terminal = Terminal::with_limits(
        COLS,
        rows,
        ScrollbackLimits {
            bytes: None,
            lines: Some(0),
        },
    );
    if case == "erase-2-seen" {
        assert!(terminal.feed(b"\x1b]133;B\x07").is_empty());
    }
    for _ in 0..4 {
        assert!(terminal.feed(&expected.bytes).is_empty());
        check(&terminal, &expected);
    }
    for round in 0..ROUNDS {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(terminal.feed(black_box(&expected.bytes)));
        }
        let elapsed = start.elapsed().as_nanos();
        check(&terminal, &expected);
        println!(
            "{{\"case\":\"{case}\",\"cols\":{COLS},\"rows\":{rows},\"round\":{round},\"iterations\":{iterations},\"input_bytes\":{},\"frames_per_feed\":{FRAMES},\"elapsed_ns\":{elapsed}}}",
            expected.bytes.len()
        );
    }
}
