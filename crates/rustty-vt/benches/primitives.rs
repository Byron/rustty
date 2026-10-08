//! Unit-level Criterion benchmarks; no PTY, session, font, renderer, or app.
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rustty_vt::{Cell, Color, Row, Screen, ScrollbackLimits, Style, Terminal, unicode};
use serde::{Deserialize, Serialize};
use std::{hint::black_box, path::Path, process::Command, time::Duration};

const COLS: u16 = 128;
const ROWS: u16 = 32;
const PATTERNS: [(&str, &str); 4] = [
    ("ascii", "abcdefgh"),
    ("chinese", "天地玄黄宇宙洪荒"),
    ("combining", "a\u{301}b\u{302}c\u{303}d\u{308}"),
    ("emoji", "👩\u{200d}💻👨\u{200d}🚀"),
];
const OPERATIONS: [&str; 9] = [
    "width",
    "print",
    "scalar",
    "read",
    "clone",
    "reflow",
    "feed",
    "stream",
    "stream_styled",
];
const HISTORY_LINES: usize = 1_024;
const PRIME_BATCHES: usize = 32;
const STREAM_RECORDS: usize = 32;

fn is_stream(operation: &str) -> bool {
    matches!(operation, "stream" | "stream_styled")
}

fn input(operation: &str, name: &str, text: &str) -> String {
    if operation == "feed" {
        return format!("\x1b[H{text}");
    }
    if !is_stream(operation) {
        return text.to_owned();
    }
    let pattern = PATTERNS.iter().find(|&&(n, _)| n == name).unwrap().1;
    // 192 display columns: one full row and a half row before CRLF.
    // Width is stated explicitly because ZWJ clusters are narrower than the
    // sum of their scalar widths.
    let repeats = match name {
        "ascii" => 24,
        "chinese" => 12,
        _ => 48,
    };
    let record = pattern.repeat(repeats);
    let mut result = String::new();
    for line in 0..STREAM_RECORDS {
        if operation == "stream_styled" {
            result.push_str(&format!(
                "\x1b[{};{}m",
                if line % 2 == 0 { 1 } else { 22 },
                31 + line % 4,
            ));
        }
        result.push_str(&record);
        if operation == "stream_styled" {
            result.push_str("\x1b[0m");
        }
        result.push_str("\r\n");
    }
    result
}

fn stream_checksum(screen: &Screen) -> u64 {
    let mut checksum = 0_u64;
    for row in screen.rows() {
        for (col, cell) in row.cells.iter().enumerate() {
            checksum = checksum.wrapping_mul(16_777_619);
            checksum = checksum.wrapping_add(cell_sum(screen, &row, col));
            if cell.codepoint().is_some() {
                let color = match row.style(col).foreground {
                    Color::Default => 0,
                    Color::Indexed(index) => u64::from(index) + 1,
                    Color::Rgb(..) => panic!("unexpected benchmark style"),
                };
                checksum = checksum
                    .wrapping_add((color + 257 * u64::from(row.style(col).bold)) * 0x11_0000);
            }
        }
    }
    checksum
}

fn check_stream(terminal: &Terminal, name: &str, styled: bool) -> u64 {
    let screen = terminal.screen();
    assert!(!screen.history().next().is_none());
    assert!(screen.history_len() <= HISTORY_LINES);
    assert_eq!(screen.cursor.row, usize::from(ROWS) - 1);
    assert_eq!(screen.cursor.col, 0);
    assert!(!screen.cursor.pending_wrap);
    // The active screen ends with 15 complete records, the preceding record's
    // final 64 columns, and one empty row. Check against the input scalars,
    // independently of either engine's printing implementation.
    let pattern = PATTERNS.iter().find(|&&(n, _)| n == name).unwrap().1;
    let repeats = match name {
        "ascii" => 24,
        "chinese" => 12,
        _ => 48,
    };
    assert_eq!(
        text_sum(screen),
        pattern.chars().map(u64::from).sum::<u64>() * (15 * repeats + repeats / 3),
    );
    for (row_index, row) in screen.rows().enumerate() {
        let record = 16 + row_index.div_ceil(2);
        for (col, _) in row
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.codepoint().is_some())
        {
            assert_eq!(
                row.style(col).foreground,
                if styled {
                    Color::Indexed(1 + (record % 4) as u8)
                } else {
                    Color::Default
                },
            );
            assert_eq!(row.style(col).bold, styled && record % 2 == 0);
        }
    }
    stream_checksum(screen)
}

fn setup_stream(bytes: &[u8]) -> Terminal {
    let mut terminal = Terminal::with_limits(
        COLS,
        ROWS,
        ScrollbackLimits {
            bytes: None,
            lines: Some(HISTORY_LINES),
        },
    );
    terminal.feed(b"\x1b[?2027h");
    // Emit 2,048 physical rows: warm allocations and force history eviction
    // before timing, rather than measuring unbounded initial growth.
    for _ in 0..PRIME_BATCHES {
        assert!(terminal.feed(bytes).is_empty());
    }
    terminal
}

// Only these two read adapters differ when copying this harness to the earlier
// String-backed implementation. The timed workloads stay identical.
fn cell_sum(screen: &Screen, row: &Row, col: usize) -> u64 {
    screen.cell_text(row, col).chars().map(u64::from).sum()
}

fn scalar(cell: &Cell) -> u64 {
    cell.codepoint().map_or(0, u64::from)
}

fn scalar_sum(screen: &Screen) -> u64 {
    screen
        .rows()
        .flat_map(|row| row.cells.iter())
        .map(scalar)
        .sum()
}

fn text_sum(screen: &Screen) -> u64 {
    screen
        .rows()
        .map(|row| {
            (0..row.cells.len())
                .map(|col| cell_sum(screen, &row, col))
                .sum::<u64>()
        })
        .sum()
}

fn width_sum(codepoints: &[char]) -> u64 {
    codepoints
        .iter()
        .copied()
        .map(unicode::codepoint_width)
        .map(u64::from)
        .sum()
}

fn overwrite(terminal: &mut Terminal, codepoints: &[char]) {
    let cursor = &mut terminal.screen_mut().cursor;
    cursor.row = 0;
    cursor.col = 0;
    cursor.pending_wrap = false;
    for &cp in codepoints {
        terminal.print(cp);
    }
}

fn setup(codepoints: &[char]) -> Terminal {
    let mut terminal = Terminal::with_limits(COLS, ROWS, ScrollbackLimits::NONE);
    terminal.feed(b"\x1b[?2027h");
    overwrite(&mut terminal, codepoints);
    let expected = codepoints.iter().copied().map(u64::from).sum::<u64>();
    assert_eq!(
        text_sum(terminal.screen()),
        expected,
        "corpus must fit without losing scalars"
    );
    assert_eq!(text_sum(&terminal.screen().snapshot_viewport()), expected);
    terminal
}

#[derive(Deserialize)]
struct NativeResult {
    engine: String,
    operation: String,
    iterations: u64,
    elapsed_ns: u64,
    units_per_iteration: u64,
    cell_bytes: usize,
    checksum: u64,
}

fn native(
    binary: &Path,
    operation: &str,
    data: &Path,
    iterations: u64,
    units: u64,
    checksum: u64,
) -> Duration {
    let output = Command::new(binary)
        .arg(operation)
        .arg(data)
        .arg(iterations.to_string())
        .output()
        .expect("run the headless Ghostty primitive benchmark");
    assert!(
        output.status.success(),
        "Ghostty {operation}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: NativeResult =
        serde_json::from_slice(&output.stdout).expect("native benchmark JSON");
    assert_eq!(result.engine, "ghostty");
    assert_eq!(result.operation, operation);
    assert_eq!(result.iterations, iterations);
    assert_eq!(result.units_per_iteration, units);
    assert_eq!(
        result.checksum, checksum,
        "engines must process identical content"
    );
    assert_eq!(result.cell_bytes, 8);
    assert!(result.elapsed_ns > 0);
    // Criterion measures the native loop's clock. Process launch, input reads,
    // setup, validation, IPC and destruction of the terminal are all excluded.
    Duration::from_nanos(result.elapsed_ns)
}

fn primitives(c: &mut Criterion) {
    let ghostty = std::env::var_os("GHOSTTY_PRIMITIVES_BIN").map(std::path::PathBuf::from);
    let corpora: Vec<_> = PATTERNS
        .iter()
        .map(|&(name, pattern)| {
            let text = pattern.repeat(128);
            let codepoints = text.chars().collect::<Vec<_>>();
            (name, text, codepoints)
        })
        .collect();
    // Prepare the exact same UTF-8 corpora for native measurements before any
    // timers start. Each process owns this tiny scratch directory.
    let data_dir = ghostty.as_ref().map(|_| {
        let directory =
            std::env::temp_dir().join(format!("rustty-primitives-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    });
    eprintln!("Rustty Cell: {} bytes", size_of::<Cell>());
    for engine in ["rustty", "ghostty"] {
        if engine == "ghostty" && ghostty.is_none() {
            continue;
        }
        for operation in OPERATIONS {
            let mut group = c.benchmark_group(format!("{engine}/{operation}"));
            for (name, text, codepoints) in &corpora {
                let input = input(operation, name, text);
                let mut terminal = if is_stream(operation) {
                    setup_stream(input.as_bytes())
                } else {
                    setup(codepoints)
                };
                let expected_text = text_sum(terminal.screen());
                let checksum = match operation {
                    "stream" | "stream_styled" => {
                        check_stream(&terminal, name, operation == "stream_styled")
                    }
                    "width" => width_sum(codepoints),
                    "scalar" => scalar_sum(terminal.screen()),
                    _ => expected_text,
                };
                let units = match operation {
                    "width" | "print" => codepoints.len() as u64,
                    "reflow" => 1,
                    "feed" | "stream" | "stream_styled" => input.len() as u64,
                    _ => u64::from(COLS) * u64::from(ROWS),
                };
                group.throughput(
                    if matches!(operation, "feed" | "stream" | "stream_styled") {
                        Throughput::Bytes(units)
                    } else {
                        Throughput::Elements(units)
                    },
                );
                if engine == "ghostty" {
                    let binary = ghostty.as_ref().unwrap();
                    let data = data_dir
                        .as_ref()
                        .unwrap()
                        .join(format!("{operation}-{name}.txt"));
                    std::fs::write(&data, &input).unwrap();
                    group.bench_function(*name, |b| {
                        b.iter_custom(|iterations| {
                            native(binary, operation, &data, iterations, units, checksum)
                        })
                    });
                } else {
                    // Dispatch outside the hot loop and make inputs/results
                    // opaque to the optimizer. Allocator counting stays in its
                    // separate test so it cannot penalize allocating revisions.
                    group.bench_function(*name, |b| match operation {
                        "width" => b.iter(|| black_box(width_sum(black_box(codepoints)))),
                        "print" => {
                            b.iter(|| overwrite(black_box(&mut terminal), black_box(codepoints)))
                        }
                        "feed" | "stream" | "stream_styled" => b.iter(|| {
                            black_box(black_box(&mut terminal).feed(black_box(input.as_bytes())))
                        }),
                        "scalar" => b.iter(|| black_box(scalar_sum(black_box(terminal.screen())))),
                        "read" => b.iter(|| black_box(text_sum(black_box(terminal.screen())))),
                        "clone" => b.iter(|| {
                            let copy = black_box(terminal.screen()).snapshot_viewport();
                            black_box(&copy);
                            // Copy destruction is included on both engines.
                            drop(copy);
                        }),
                        "reflow" => b.iter(|| {
                            let terminal = black_box(&mut terminal);
                            terminal.resize(COLS / 2, ROWS);
                            terminal.resize(COLS, ROWS);
                        }),
                        _ => unreachable!(),
                    });
                }
                if is_stream(operation) {
                    assert_eq!(
                        check_stream(&terminal, name, operation == "stream_styled"),
                        checksum
                    );
                }
                assert_eq!(
                    text_sum(terminal.screen()),
                    expected_text,
                    "operation changed text"
                );
            }
            group.finish();
        }
    }
    if let Some(directory) = data_dir {
        std::fs::remove_dir_all(directory).unwrap();
    }
}

// Six supplementary Rust-only workloads keep chunk boundaries visible without
// changing any of the saved Rustty/Ghostty primitive comparisons.
const MIXED_CELLS: [&str; 16] = [
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "天",
    "",
    "地",
    "",
    "a\u{301}",
    "b\u{302}",
    "👩\u{200d}💻",
    "",
];

fn mixed_input(stream: bool) -> String {
    let pattern = MIXED_CELLS.concat();
    let mut bytes = if stream {
        String::new()
    } else {
        "\x1b[H".into()
    };
    for _ in 0..if stream { STREAM_RECORDS } else { 1 } {
        for unit in 0..if stream { 12 } else { 128 } {
            bytes.push_str(&format!(
                "\x1b[{};{}m{pattern}",
                if unit % 2 == 0 { 1 } else { 22 },
                31 + unit % 4,
            ));
        }
        bytes.push_str("\x1b[0m");
        if stream {
            bytes.push_str("\r\n");
        }
    }
    bytes
}

fn check_mixed(terminal: &Terminal, stream: bool) {
    let screen = terminal.screen();
    assert_eq!(screen.height(), usize::from(ROWS));
    assert_eq!(screen.cursor.row, if stream { 31 } else { 15 });
    assert_eq!(screen.cursor.col, if stream { 0 } else { 127 });
    assert_eq!(screen.cursor.pending_wrap, !stream);
    assert_eq!(screen.cursor.style, Style::default());
    assert_eq!(screen.history().next().is_none(), !stream);
    assert!(screen.history_len() <= HISTORY_LINES);
    let total_rows = screen.history_len() + screen.height();
    for (index, row) in screen.all_rows().enumerate() {
        assert_eq!(row.cells.len(), usize::from(COLS));
        let columns = if stream {
            if index == total_rows - 1 {
                0
            } else if (total_rows - index) % 2 == 1 {
                128
            } else {
                64
            }
        } else if index < 16 {
            128
        } else {
            0
        };
        if stream {
            assert_eq!(row.wrapped, columns == 128);
            assert_eq!(row.wrap_continuation, columns == 64);
        } else {
            assert_eq!(row.wrapped, index < 15);
            assert_eq!(row.wrap_continuation, (1..16).contains(&index));
        }
        for (col, cell) in row.cells.iter().enumerate() {
            let populated = col < columns;
            assert_eq!(
                &*screen.cell_text(row, col),
                if populated { MIXED_CELLS[col % 16] } else { "" }
            );
            assert_eq!(
                cell.width(),
                if !populated {
                    1
                } else {
                    match col % 16 {
                        8 | 10 | 14 => 2,
                        9 | 11 | 15 => 0,
                        _ => 1,
                    }
                }
            );
            assert_eq!(
                row.style(col),
                if populated {
                    Style {
                        foreground: Color::Indexed(1 + (col / 16 % 4) as u8),
                        bold: col / 16 % 2 == 0,
                        ..Style::default()
                    }
                } else {
                    Style::default()
                }
            );
        }
    }
}

fn chunked_input(c: &mut Criterion) {
    for stream in [false, true] {
        let input = mixed_input(stream);
        let setup = || {
            if stream {
                setup_stream(input.as_bytes())
            } else {
                let mut terminal = Terminal::with_limits(COLS, ROWS, ScrollbackLimits::NONE);
                terminal.feed(b"\x1b[?2027h");
                assert!(terminal.feed(input.as_bytes()).is_empty());
                terminal
            }
        };
        let mut reference = setup();
        assert!(reference.feed(input.as_bytes()).is_empty());
        check_mixed(&reference, stream);
        let mut group = c.benchmark_group(if stream {
            "rustty/chunked_stream_mixed"
        } else {
            "rustty/chunked_feed_mixed"
        });
        group.throughput(Throughput::Bytes(input.len() as u64));
        for (name, chunk_size) in [("whole", input.len()), ("7_bytes", 7), ("4_KiB", 4096)] {
            let mut terminal = setup();
            for chunk in input.as_bytes().chunks(chunk_size) {
                assert!(terminal.feed(chunk).is_empty());
            }
            let actual = terminal.screen();
            let expected = reference.screen();
            assert_eq!(actual.cursor, expected.cursor);
            assert_eq!(actual.history_len(), expected.history_len());
            for (row, expected_row) in actual.all_rows().zip(expected.all_rows()) {
                assert_eq!(row.wrapped, expected_row.wrapped);
                assert_eq!(row.wrap_continuation, expected_row.wrap_continuation);
                for (col, (cell, expected_cell)) in
                    row.cells.iter().zip(expected_row.cells).enumerate()
                {
                    assert_eq!(
                        &*actual.cell_text(row, col),
                        &*expected.cell_text(expected_row, col)
                    );
                    assert_eq!(cell.width(), expected_cell.width());
                    assert_eq!(row.style(col), expected_row.style(col));
                }
            }
            group.bench_function(name, |b| {
                b.iter(|| {
                    let terminal = black_box(&mut terminal);
                    for chunk in black_box(input.as_bytes()).chunks(chunk_size) {
                        black_box(terminal.feed(chunk));
                    }
                })
            });
            check_mixed(&terminal, stream);
        }
        group.finish();
    }
}

// Keep populated history below its limit at both widths, so each round trip
// reflows the same records without accumulating evictions or new input.
const REFLOW_BATCHES: usize = 8;

fn check_history_reflow(terminal: &Terminal, name: &str, columns: usize) {
    let screen = terminal.screen();
    let records = REFLOW_BATCHES * STREAM_RECORDS;
    let record_rows = 192_usize.div_ceil(columns);
    assert_eq!(screen.height(), usize::from(ROWS));
    assert_eq!(
        screen.history_len(),
        records * record_rows + 1 - usize::from(ROWS)
    );
    assert!(screen.history_len() < HISTORY_LINES);
    assert_eq!(screen.cursor.row, usize::from(ROWS) - 1);
    assert_eq!(screen.cursor.col, 0);
    assert!(!screen.cursor.pending_wrap);
    let cells: &[&str] = match name {
        "ascii" => &["a", "b", "c", "d", "e", "f", "g", "h"],
        "chinese" => &[
            "天", "", "地", "", "玄", "", "黄", "", "宇", "", "宙", "", "洪", "", "荒", "",
        ],
        "combining" => &["a\u{301}", "b\u{302}", "c\u{303}", "d\u{308}"],
        "emoji" => &["👩\u{200d}💻", "", "👨\u{200d}🚀", ""],
        _ => unreachable!(),
    };
    for (index, row) in screen.all_rows().enumerate() {
        let record_row = index % record_rows;
        let populated = index < records * record_rows;
        let used = if populated {
            (192 - record_row * columns).min(columns)
        } else {
            0
        };
        assert_eq!(row.cells.len(), columns);
        assert_eq!(row.wrapped, populated && record_row + 1 < record_rows);
        assert_eq!(row.wrap_continuation, populated && record_row > 0);
        for (col, cell) in row.cells.iter().enumerate() {
            let text = if col < used {
                cells[col % cells.len()]
            } else {
                ""
            };
            assert_eq!(&*screen.cell_text(row, col), text);
            assert_eq!(row.style(col), Style::default());
            assert_eq!(
                cell.width(),
                if col >= used {
                    1
                } else if text.is_empty() {
                    0
                } else if matches!(name, "chinese" | "emoji") {
                    2
                } else {
                    1
                }
            );
        }
    }
}

fn history_reflow(c: &mut Criterion) {
    let mut group = c.benchmark_group("rustty/reflow_history");
    group.throughput(Throughput::Elements(1));
    for (name, pattern) in PATTERNS {
        let input = input("stream", name, pattern);
        let mut terminal = Terminal::with_limits(
            COLS,
            ROWS,
            ScrollbackLimits {
                bytes: None,
                lines: Some(HISTORY_LINES),
            },
        );
        terminal.feed(b"\x1b[?2027h");
        for _ in 0..REFLOW_BATCHES {
            assert!(terminal.feed(input.as_bytes()).is_empty());
        }
        check_history_reflow(&terminal, name, usize::from(COLS));
        // Prime a complete round trip and validate the expanded history too.
        terminal.resize(COLS / 2, ROWS);
        check_history_reflow(&terminal, name, usize::from(COLS / 2));
        terminal.resize(COLS, ROWS);
        check_history_reflow(&terminal, name, usize::from(COLS));
        group.bench_function(name, |b| {
            b.iter(|| {
                let terminal = black_box(&mut terminal);
                terminal.resize(COLS / 2, ROWS);
                terminal.resize(COLS, ROWS);
            })
        });
        check_history_reflow(&terminal, name, usize::from(COLS));
    }
    group.finish();
}

fn styled_history_reflow(c: &mut Criterion) {
    let ghostty = std::env::var_os("GHOSTTY_PRIMITIVES_BIN").map(std::path::PathBuf::from);
    let mut native_inputs = Vec::new();
    let mut group = c.benchmark_group("rustty/reflow_history_styled");
    for records in [1_024, 8_192, 32_768] {
        let mut terminal = Terminal::with_limits(
            COLS,
            ROWS,
            ScrollbackLimits {
                bytes: None,
                lines: None,
            },
        );
        let text = "abcdefgh".repeat(24);
        let mut input = String::new();
        for line in 0..records {
            let record = format!(
                "\x1b[{};{}m{text}\x1b[0m\r\n",
                if line % 2 == 0 { 1 } else { 22 },
                31 + line % 4,
            );
            terminal.feed(record.as_bytes());
            input.push_str(&record);
        }
        let check = |terminal: &Terminal, columns: usize| {
            let screen = terminal.screen();
            let record_rows = 192_usize.div_ceil(columns);
            assert_eq!(
                screen.history_len(),
                records * record_rows + 1 - usize::from(ROWS)
            );
            let mut checksum = 0_u64;
            for (index, row) in screen.all_rows().enumerate().take(records * record_rows) {
                let record = index / record_rows;
                let used = (192 - index % record_rows * columns).min(columns);
                for col in 0..used {
                    assert_eq!(
                        row.cells[col].codepoint(),
                        Some(b"abcdefgh"[col % 8] as char)
                    );
                    let style = row.style(col);
                    assert_eq!(style.foreground, Color::Indexed(1 + (record % 4) as u8));
                    assert_eq!(style.bold, record.is_multiple_of(2));
                    checksum = checksum.wrapping_add(
                        u64::from(b"abcdefgh"[col % 8])
                            + (2 + (record % 4) as u64 + 257 * u64::from(style.bold)) * 0x11_0000,
                    );
                }
            }
            checksum
        };
        let checksum = check(&terminal, usize::from(COLS));
        terminal.resize(COLS / 2, ROWS);
        assert_eq!(check(&terminal, usize::from(COLS / 2)), checksum);
        terminal.resize(COLS, ROWS);
        assert_eq!(check(&terminal, usize::from(COLS)), checksum);
        group.throughput(Throughput::Elements(records as u64));
        group.bench_function(records.to_string(), |b| {
            b.iter(|| {
                let terminal = black_box(&mut terminal);
                terminal.resize(COLS / 2, ROWS);
                terminal.resize(COLS, ROWS);
            })
        });
        assert_eq!(check(&terminal, usize::from(COLS)), checksum);
        native_inputs.push((records, input, checksum));
    }
    group.finish();
    if let Some(binary) = ghostty {
        let directory =
            std::env::temp_dir().join(format!("rustty-styled-reflow-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut group = c.benchmark_group("ghostty/reflow_history_styled");
        for (records, input, checksum) in native_inputs {
            let data = directory.join(format!("{records}.txt"));
            std::fs::write(&data, input).unwrap();
            group.throughput(Throughput::Elements(records as u64));
            group.bench_function(records.to_string(), |b| {
                b.iter_custom(|iterations| {
                    native(
                        &binary,
                        "reflow_history_styled",
                        &data,
                        iterations,
                        records as u64,
                        checksum,
                    )
                })
            });
        }
        group.finish();
        std::fs::remove_dir_all(directory).unwrap();
    }
}

// Public synthetic records keep logical content fixed across engine, width and
// history size. Widths are explicit so neither engine is the correctness oracle.
#[derive(Clone, Serialize)]
struct ContentHistory {
    pattern: &'static str,
    records: usize,
    styled: bool,
    link_repeats: usize,
    tracked: bool,
    narrow: u16,
}

#[derive(Clone, Copy)]
struct ExpectedHistoryCell {
    text: &'static str,
    width: u8,
    spacer: bool,
    linked: bool,
}

impl ContentHistory {
    fn units(&self) -> &'static [(&'static str, u8)] {
        match self.pattern {
            "ascii" => &[
                ("a", 1),
                ("b", 1),
                ("c", 1),
                ("d", 1),
                ("e", 1),
                ("f", 1),
                ("g", 1),
                ("h", 1),
            ],
            "chinese" => &[
                ("天", 2),
                ("地", 2),
                ("玄", 2),
                ("黄", 2),
                ("宇", 2),
                ("宙", 2),
                ("洪", 2),
                ("荒", 2),
            ],
            "combining" => &[
                ("a\u{301}", 1),
                ("b\u{302}", 1),
                ("c\u{303}", 1),
                ("d\u{308}", 1),
            ],
            "emoji" => &[("👩\u{200d}💻", 2), ("👨\u{200d}🚀", 2)],
            "mixed" => &[("a", 1), ("界", 2), ("e\u{301}", 1), ("👩\u{200d}💻", 2)],
            _ => unreachable!(),
        }
    }

    fn repeats(&self) -> usize {
        let width: usize = self
            .units()
            .iter()
            .map(|(_, width)| usize::from(*width))
            .sum();
        assert_eq!(192 % width, 0);
        192 / width
    }

    fn style(&self, record: usize) -> Style {
        Style {
            foreground: if self.styled {
                Color::Indexed(1 + (record % 4) as u8)
            } else {
                Color::Default
            },
            bold: self.styled && record.is_multiple_of(2),
            ..Style::default()
        }
    }

    fn layout(&self, columns: usize) -> Vec<Vec<ExpectedHistoryCell>> {
        let blank = ExpectedHistoryCell {
            text: "",
            width: 1,
            spacer: false,
            linked: false,
        };
        let mut result = vec![vec![blank; columns]];
        let mut col = 0;
        for repeat in 0..self.repeats() {
            for &(text, width) in self.units() {
                if col + usize::from(width) > columns {
                    if col < columns {
                        result.last_mut().unwrap()[col].spacer = true;
                    }
                    result.push(vec![blank; columns]);
                    col = 0;
                }
                let row = result.last_mut().unwrap();
                row[col] = ExpectedHistoryCell {
                    text,
                    width,
                    spacer: false,
                    linked: repeat < self.link_repeats,
                };
                if width == 2 {
                    row[col + 1].width = 0;
                    row[col + 1].linked = repeat < self.link_repeats;
                }
                col += usize::from(width);
            }
        }
        result
    }

    fn checksum(&self) -> u64 {
        let mut checksum = 0_u64;
        for record in 0..self.records {
            let color = if self.styled {
                2 + record as u64 % 4
            } else {
                0
            };
            let style = (color + 257 * u64::from(self.styled && record % 2 == 0)) * 0x11_0000;
            for _ in 0..self.repeats() {
                for &(text, _) in self.units() {
                    checksum = checksum
                        .wrapping_mul(16_777_619)
                        .wrapping_add(text.chars().map(u64::from).sum::<u64>())
                        .wrapping_add(style);
                }
            }
        }
        checksum
    }

    fn setup(&self) -> (Terminal, Vec<rustty_vt::TrackedPoint>) {
        let mut terminal = Terminal::with_limits(
            COLS,
            ROWS,
            ScrollbackLimits {
                bytes: None,
                lines: None,
            },
        );
        terminal.feed(b"\x1b[?2027h");
        let pattern: String = self.units().iter().map(|(text, _)| *text).collect();
        assert!(self.link_repeats <= self.repeats());
        let linked = pattern.repeat(self.link_repeats);
        let rest = pattern.repeat(self.repeats() - self.link_repeats);
        for record in 0..self.records {
            if self.styled {
                terminal.feed(
                    format!(
                        "\x1b[{};{}m",
                        if record % 2 == 0 { 1 } else { 22 },
                        31 + record % 4
                    )
                    .as_bytes(),
                );
            }
            if self.link_repeats > 0 {
                terminal.feed(
                    format!("\x1b]8;id={record};https://example.test/reflow/{record}\x1b\\")
                        .as_bytes(),
                );
                terminal.feed(linked.as_bytes());
                terminal.feed(b"\x1b]8;;\x1b\\");
            }
            terminal.feed(rest.as_bytes());
            terminal.feed(b"\x1b[0m\r\n");
        }
        let stride = self.layout(usize::from(COLS)).len();
        let anchors = if self.tracked {
            [0, self.records / 2, self.records - 1]
                .into_iter()
                .map(|record| {
                    let point = terminal.screen().point(record * stride, 0).unwrap();
                    terminal.screen_mut().track(point)
                })
                .collect()
        } else {
            Vec::new()
        };
        (terminal, anchors)
    }

    fn check(&self, terminal: &Terminal, columns: usize, anchors: &[rustty_vt::TrackedPoint]) {
        let layout = self.layout(columns);
        let populated = self.records * layout.len();
        let screen = terminal.screen();
        assert_eq!(
            screen.history_len() + screen.height(),
            (populated + 1).max(usize::from(ROWS))
        );
        assert_eq!(
            (
                screen.cursor.row,
                screen.cursor.col,
                screen.cursor.pending_wrap
            ),
            (usize::from(ROWS) - 1, 0, false)
        );
        for (index, row) in screen.all_rows().enumerate() {
            let record = index / layout.len();
            let offset = index % layout.len();
            assert_eq!(row.wrapped, index < populated && offset + 1 < layout.len());
            assert_eq!(row.wrap_continuation, index < populated && offset != 0);
            assert_eq!(row.cells.len(), columns);
            let link = (self.link_repeats > 0).then(|| {
                rustty_vt::HyperlinkData::new(
                    format!("https://example.test/reflow/{record}").as_bytes(),
                    Some(rustty_vt::HyperlinkId::Explicit(
                        record.to_string().into_bytes(),
                    )),
                )
            });
            for (col, cell) in row.cells.iter().enumerate() {
                let expected = if index < populated {
                    layout[offset][col]
                } else {
                    ExpectedHistoryCell {
                        text: "",
                        width: 1,
                        spacer: false,
                        linked: false,
                    }
                };
                assert_eq!(
                    row.text(col).as_str(),
                    expected.text,
                    "record={record} row={offset} col={col}"
                );
                assert_eq!(cell.width(), expected.width);
                assert_eq!(cell.spacer_head(), expected.spacer);
                assert_eq!(cell.has_grapheme(), expected.text.chars().count() > 1);
                // Printing and reflow can assign different attributes to empty
                // spacer-head padding. Real heads and tails must retain all data.
                if expected.spacer {
                    continue;
                }
                let used = !expected.text.is_empty() || expected.width == 0;
                assert_eq!(
                    row.style(col),
                    if used {
                        self.style(record)
                    } else {
                        Style::default()
                    }
                );
                assert_eq!(
                    row.hyperlink(col),
                    if expected.linked { link.as_ref() } else { None },
                    "{} cols={columns} record={record} row={offset} col={col}",
                    self.pattern
                );
            }
        }
        for (&anchor, record) in anchors.iter().zip([0, self.records / 2, self.records - 1]) {
            assert_eq!(
                screen.resolve(anchor),
                screen.point(record * layout.len(), 0)
            );
        }
    }
}

fn content_history_reflow(c: &mut Criterion) {
    let mut cases = Vec::new();
    for pattern in ["ascii", "chinese", "combining", "emoji", "mixed"] {
        for styled in [false, true] {
            if matches!(pattern, "ascii" | "mixed") && !styled {
                continue;
            }
            for records in [1_024, 8_192, 32_768] {
                cases.push((
                    format!("{pattern}-{}", if styled { "styled" } else { "plain" }),
                    ContentHistory {
                        pattern,
                        records,
                        styled,
                        link_repeats: 0,
                        tracked: false,
                        narrow: 64,
                    },
                ));
            }
        }
    }
    for (label, pattern, link_repeats, tracked, narrow) in [
        ("mixed-linked", "mixed", 1, false, 64),
        ("mixed-tracked", "mixed", 0, true, 64),
        ("chinese-odd-width", "chinese", 0, false, 63),
        ("mixed-odd-width", "mixed", 0, false, 63),
    ] {
        cases.push((
            label.to_owned(),
            ContentHistory {
                pattern,
                records: 8_192,
                styled: true,
                link_repeats,
                tracked,
                narrow,
            },
        ));
    }
    // Both engines currently exhaust a per-page link map during this dense
    // input. Keep the failing correctness probe explicit; never time lost links.
    if std::env::var_os("RUSTTY_REFLOW_DENSE_LINKS").is_some() {
        cases.push((
            "mixed-dense-links".to_owned(),
            ContentHistory {
                pattern: "mixed",
                records: 8_192,
                styled: true,
                link_repeats: 32,
                tracked: false,
                narrow: 64,
            },
        ));
    }
    let ghostty = std::env::var_os("GHOSTTY_PRIMITIVES_BIN");
    let directory =
        std::env::temp_dir().join(format!("rustty-content-reflow-{}", std::process::id()));
    for engine in ["rustty", "ghostty"] {
        if engine == "ghostty" && ghostty.is_none() {
            continue;
        }
        let mut group = c.benchmark_group(format!("{engine}/reflow_history_content"));
        for (label, case) in &cases {
            group.throughput(Throughput::Elements(case.records as u64));
            group.bench_with_input(BenchmarkId::new(label, case.records), case, |b, case| {
                if engine == "rustty" {
                    let (mut terminal, anchors) = case.setup();
                    eprintln!(
                        "Validating {} records of {} after input",
                        case.records, label
                    );
                    case.check(&terminal, usize::from(COLS), &anchors);
                    terminal.resize(case.narrow, ROWS);
                    eprintln!("Validating narrowed history at {} columns", case.narrow);
                    case.check(&terminal, usize::from(case.narrow), &anchors);
                    terminal.resize(COLS, ROWS);
                    eprintln!("Validating primed history at {COLS} columns");
                    case.check(&terminal, usize::from(COLS), &anchors);
                    b.iter(|| {
                        let terminal = black_box(&mut terminal);
                        terminal.resize(case.narrow, ROWS);
                        terminal.resize(COLS, ROWS);
                    });
                    case.check(&terminal, usize::from(COLS), &anchors);
                } else {
                    std::fs::create_dir_all(&directory).unwrap();
                    let data = directory.join(format!("{label}-{}.json", case.records));
                    std::fs::write(&data, serde_json::to_vec(case).unwrap()).unwrap();
                    let checksum = case.checksum();
                    b.iter_custom(|iterations| {
                        native(
                            Path::new(ghostty.as_ref().unwrap()),
                            "reflow_history_content",
                            &data,
                            iterations,
                            case.records as u64,
                            checksum,
                        )
                    });
                }
            });
        }
        group.finish();
    }
    if directory.exists() {
        std::fs::remove_dir_all(directory).unwrap();
    }
}

// Optional local capture; the harness never copies its private input into the
// repository or includes it in benchmark results.
fn captured_scrollback_reflow(c: &mut Criterion) {
    let Some(data) = std::env::var_os("RUSTTY_REFLOW_INPUT").map(std::path::PathBuf::from) else {
        return;
    };
    let input = std::fs::read(&data).unwrap();
    let mut terminal = Terminal::with_limits(
        COLS,
        ROWS,
        ScrollbackLimits {
            bytes: None,
            lines: None,
        },
    );
    terminal.feed(b"\x1b[?2027h");
    for chunk in input.chunks(64 * 1024) {
        terminal.feed(chunk);
    }
    let check = |terminal: &Terminal| {
        let mut checksum = 0_u64;
        for row in terminal.screen().all_rows() {
            for (col, cell) in row.cells.iter().enumerate() {
                if cell.codepoint().is_none() {
                    continue;
                }
                let style = row.style(col);
                let color = match style.foreground {
                    Color::Default => 0,
                    Color::Indexed(value) => u64::from(value) + 1,
                    Color::Rgb(r, g, b) => {
                        257 + (u64::from(r) << 16) + (u64::from(g) << 8) + u64::from(b)
                    }
                };
                checksum = checksum
                    .wrapping_mul(16_777_619)
                    .wrapping_add(row.text(col).chars().map(u64::from).sum::<u64>())
                    .wrapping_add((color + 257 * u64::from(style.bold)) * 0x11_0000);
            }
        }
        checksum
    };
    let checksum = check(&terminal);
    eprintln!(
        "Captured scrollback: {} history rows",
        terminal.screen().history_len()
    );
    terminal.resize(COLS / 2, ROWS);
    assert_eq!(check(&terminal), checksum);
    terminal.resize(COLS, ROWS);
    assert_eq!(check(&terminal), checksum);
    let mut group = c.benchmark_group("rustty/reflow_scrollback");
    group.bench_function("capture", |b| {
        b.iter(|| {
            let terminal = black_box(&mut terminal);
            terminal.resize(COLS / 2, ROWS);
            terminal.resize(COLS, ROWS);
        })
    });
    assert_eq!(check(&terminal), checksum);
    group.finish();
    if let Some(binary) = std::env::var_os("GHOSTTY_PRIMITIVES_BIN") {
        let mut group = c.benchmark_group("ghostty/reflow_scrollback");
        group.bench_function("capture", |b| {
            b.iter_custom(|iterations| {
                native(
                    Path::new(&binary),
                    "reflow_scrollback",
                    &data,
                    iterations,
                    1,
                    checksum,
                )
            })
        });
        group.finish();
    }
}

// Match the app's default owned-history cap while retaining the same stream
// inputs and native line limit as the uncapped primitive comparisons.
fn memory_capped_streams(c: &mut Criterion) {
    const MEMORY_LIMIT: usize = 50_000_000;
    for operation in ["stream", "stream_styled"] {
        let mut group = c.benchmark_group(format!("rustty/{operation}_memory_capped"));
        for (name, pattern) in PATTERNS {
            let input = input(operation, name, pattern);
            let mut reference = setup_stream(input.as_bytes());
            let mut terminal = setup_stream(input.as_bytes());
            terminal.set_scrollback_memory_limit(Some(MEMORY_LIMIT));
            assert_eq!(
                terminal.screen().history_len(),
                reference.screen().history_len()
            );
            assert!(reference.feed(input.as_bytes()).is_empty());
            assert!(terminal.feed(input.as_bytes()).is_empty());
            assert_eq!(
                terminal.screen().history_len(),
                reference.screen().history_len()
            );
            let checksum = check_stream(&reference, name, operation == "stream_styled");
            assert_eq!(
                check_stream(&terminal, name, operation == "stream_styled"),
                checksum
            );
            assert!(terminal.screen().history_bytes() <= MEMORY_LIMIT);
            group.throughput(Throughput::Bytes(input.len() as u64));
            group.bench_function(name, |b| {
                b.iter(|| black_box(black_box(&mut terminal).feed(black_box(input.as_bytes()))))
            });
            assert_eq!(
                check_stream(&terminal, name, operation == "stream_styled"),
                checksum
            );
            assert!(terminal.screen().history_bytes() <= MEMORY_LIMIT);
        }
        group.finish();
    }
}

criterion_group!(
    benches,
    primitives,
    chunked_input,
    history_reflow,
    styled_history_reflow,
    content_history_reflow,
    captured_scrollback_reflow,
    memory_capped_streams
);
criterion_main!(benches);
