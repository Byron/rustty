//! Shared generated-file throughput: resident 64 KiB chunks, reset/checks untimed.
//! Native terminal-stream also reads files while timing; this primitive adapter
//! loads them first so both engines measure parsing and terminal state updates.
use criterion::{Criterion, Throughput};
use rustty_vt::{Color, HyperlinkId, ScrollbackLimits, Style, Terminal};
use serde::{Deserialize, Serialize};
use std::{
    hint::black_box,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GraphemeWidth {
    #[default]
    Legacy,
    Unicode,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub data: std::path::PathBuf,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_bytes: usize,
    pub grapheme_width: GraphemeWidth,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "file".into(),
            data: Default::default(),
            cols: 120,
            rows: 80,
            scrollback_bytes: 10_000,
            grapheme_width: GraphemeWidth::Legacy,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Result {
    pub engine: String,
    pub operation: String,
    pub name: String,
    pub iterations: u64,
    pub elapsed_ns: u64,
    pub units_per_iteration: u64,
    pub cell_bytes: usize,
    pub checksum: u64,
    pub history_rows: usize,
    /// Current native page charge, not peak allocation or RSS.
    pub page_bytes: usize,
    pub page_count: usize,
    /// Rust page storage estimate, excluding allocator metadata and RSS.
    pub owned_bytes: Option<usize>,
}

pub fn load(path: &Path) -> (Config, Vec<u8>) {
    let config: Config = serde_json::from_slice(&std::fs::read(path).expect("read stream config"))
        .expect("parse stream config");
    assert!(
        config.cols > 0 && config.rows > 0,
        "terminal dimensions must be positive"
    );
    assert!(
        std::fs::metadata(&config.data)
            .expect("stat stream data")
            .len()
            <= 128 * 1024 * 1024
    );
    let bytes = std::fs::read(&config.data).expect("read stream data");
    assert!(!bytes.is_empty(), "stream data must not be empty");
    (config, bytes)
}

fn mix(sum: &mut u64, value: u64) {
    *sum = sum.wrapping_mul(16_777_619).wrapping_add(value);
}

fn color(color: Color) -> u64 {
    match color {
        Color::Default => 0,
        Color::Indexed(i) => 1 | u64::from(i) << 8,
        Color::Rgb(r, g, b) => 2 | u64::from(r) << 8 | u64::from(g) << 16 | u64::from(b) << 24,
    }
}

fn style(sum: &mut u64, value: Style) {
    for channel in [value.foreground, value.background, value.underline_color] {
        mix(sum, color(channel));
    }
    let mut flags = (value.underline as u64) << 8;
    for (bit, set) in [
        value.bold,
        value.italic,
        value.faint,
        value.blink,
        value.inverse,
        value.invisible,
        value.strikethrough,
        value.overline,
    ]
    .into_iter()
    .enumerate()
    {
        flags |= u64::from(set) << bit;
    }
    mix(sum, flags);
}

fn string(sum: &mut u64, bytes: &[u8]) {
    mix(sum, bytes.len() as u64);
    for &byte in bytes {
        mix(sum, u64::from(byte));
    }
}

// Visible logical state; storage IDs/hints and numeric implicit link identities
// are checked by the parity suite, not this throughput checksum.
pub fn checksum(terminal: &Terminal) -> u64 {
    let screen = terminal.screen();
    let mut sum = 0;
    for value in [
        screen.cursor.col as u64,
        screen.cursor.row as u64,
        u64::from(screen.cursor.pending_wrap),
        u64::from(terminal.modes.dec(2027)),
    ] {
        mix(&mut sum, value);
    }
    style(&mut sum, screen.cursor.style);
    for row in screen.rows() {
        mix(&mut sum, u64::from(row.wrapped));
        mix(&mut sum, u64::from(row.wrap_continuation));
        for (col, cell) in row.cells.iter().enumerate() {
            let text = screen.cell_text(row, col);
            mix(&mut sum, text.chars().count() as u64);
            for cp in text.chars() {
                mix(&mut sum, u64::from(cp));
            }
            mix(&mut sum, u64::from(cell.width()));
            mix(&mut sum, u64::from(cell.spacer_head()));
            mix(&mut sum, u64::from(cell.protected()));
            mix(&mut sum, cell.semantic() as u64);
            style(&mut sum, row.style(col));
            if let Some(link) = row.hyperlink(col) {
                mix(&mut sum, 1);
                string(&mut sum, link.uri_bytes());
                match &link.id {
                    Some(HyperlinkId::Explicit(id)) => {
                        mix(&mut sum, 2);
                        string(&mut sum, id);
                    }
                    Some(HyperlinkId::Implicit(_)) | None => mix(&mut sum, 1),
                }
            } else {
                mix(&mut sum, 0);
            }
        }
    }
    sum
}

fn feed(terminal: &mut Terminal, bytes: &[u8]) {
    for chunk in bytes.chunks(64 * 1024) {
        black_box(terminal.feed(black_box(chunk)));
    }
}

pub fn run(config: &Config, bytes: &[u8], iterations: u64) -> Result {
    assert!(iterations > 0);
    let mut terminal = Terminal::with_limits(
        config.cols,
        config.rows,
        ScrollbackLimits {
            bytes: Some(config.scrollback_bytes),
            lines: None,
        },
    );
    assert!(terminal.set_default_mode(true, 2027, config.grapheme_width == GraphemeWidth::Unicode));
    terminal.reset_from_host();
    feed(&mut terminal, bytes);
    let expected = checksum(&terminal);
    let mut elapsed = Duration::ZERO;
    for _ in 0..iterations {
        terminal.reset_from_host();
        assert_eq!(
            terminal.modes.dec(2027),
            config.grapheme_width == GraphemeWidth::Unicode
        );
        let start = Instant::now();
        feed(black_box(&mut terminal), bytes);
        elapsed += start.elapsed();
        assert_eq!(
            checksum(&terminal),
            expected,
            "reset replay changed visible output"
        );
    }
    let screen = terminal.screen();
    Result {
        engine: "rustty".into(),
        operation: "stream_file".into(),
        name: config.name.clone(),
        iterations,
        elapsed_ns: elapsed.as_nanos().try_into().unwrap(),
        units_per_iteration: bytes.len() as u64,
        cell_bytes: size_of::<rustty_vt::Cell>(),
        checksum: expected,
        history_rows: screen.history_len(),
        page_bytes: screen.storage_bytes(),
        page_count: screen.page_allocations().count(),
        owned_bytes: Some(screen.owned_bytes()),
    }
}

pub fn benchmarks(c: &mut Criterion) {
    let Some(path) = std::env::var_os("RUSTTY_STREAM_CONFIG") else {
        return;
    };
    let (config, bytes) = load(Path::new(&path));
    let expected = run(&config, &bytes, 1);
    eprintln!(
        "stream_file_check {}",
        serde_json::to_string(&expected).unwrap()
    );
    let mut group = c.benchmark_group("rustty/stream_file");
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function(&config.name, |b| {
        b.iter_custom(|iterations| {
            Duration::from_nanos(run(&config, &bytes, iterations).elapsed_ns)
        })
    });
    group.finish();
    if let Some(binary) = std::env::var_os("GHOSTTY_PRIMITIVES_BIN") {
        let native = |iterations: u64| {
            let output = Command::new(&binary)
                .arg("stream_file")
                .arg(&path)
                .arg(iterations.to_string())
                .output()
                .expect("run native stream benchmark");
            assert!(
                output.status.success(),
                "native stream benchmark: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: Result =
                serde_json::from_slice(&output.stdout).expect("native stream JSON");
            assert_eq!(result.engine, "ghostty");
            assert_eq!(result.operation, "stream_file");
            assert_eq!(result.iterations, iterations);
            assert_eq!(result.units_per_iteration, bytes.len() as u64);
            assert_eq!(
                result.checksum, expected.checksum,
                "native visible state differs"
            );
            result
        };
        eprintln!(
            "stream_file_check {}",
            serde_json::to_string(&native(1)).unwrap()
        );
        let mut group = c.benchmark_group("ghostty/stream_file");
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(&config.name, |b| {
            b.iter_custom(|iterations| Duration::from_nanos(native(iterations).elapsed_ns))
        });
        group.finish();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reset_and_chunking_retain_styles_widths_links_and_mode_defaults() {
        use super::*;

        let mut bytes = vec![b' '; 64 * 1024 - 1];
        bytes.extend_from_slice(
            b"\x1bc\x1b[1;3;38;2;51;102;153mA\x1b[0m\x1b]8;id=x;https://example.test\x07",
        );
        bytes.extend_from_slice("界e\u{301}".as_bytes());
        bytes.extend_from_slice(b"\x1b]8;;\x07\r\n\x1b[44m\x1b[K\x1b[0m");
        for width in [GraphemeWidth::Legacy, GraphemeWidth::Unicode] {
            for scrollback_bytes in [0, 10_000, 50_000_000] {
                let config = Config {
                    cols: 12,
                    rows: 4,
                    grapheme_width: width,
                    scrollback_bytes,
                    ..Default::default()
                };
                let result = run(&config, &bytes, 2);
                let mut scalar = Terminal::with_limits(
                    config.cols,
                    config.rows,
                    ScrollbackLimits {
                        bytes: Some(scrollback_bytes),
                        lines: None,
                    },
                );
                assert!(scalar.set_default_mode(true, 2027, width == GraphemeWidth::Unicode));
                for byte in &bytes {
                    scalar.feed(std::slice::from_ref(byte));
                }
                assert_eq!(result.checksum, checksum(&scalar));
                assert_eq!(
                    scalar.screen().row(0).style(0).foreground,
                    Color::Rgb(51, 102, 153)
                );
                assert_eq!(scalar.screen().row(0).cells[1].width(), 2);
                assert_eq!(
                    scalar.screen().row(0).hyperlink(1).unwrap().uri_bytes(),
                    b"https://example.test"
                );
                assert_eq!(
                    scalar.screen().row(1).style(0).background,
                    Color::Indexed(4)
                );
                scalar.feed(b"Z");
                assert_ne!(result.checksum, checksum(&scalar));
            }
        }
    }
}
