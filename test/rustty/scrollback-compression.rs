//! Live packed-history memory and on-demand access costs; no PTY or renderer.
use rustty_vt::{ScrollbackLimits, Terminal, snapshot};
use serde_json::json;
use std::{hint::black_box, time::Instant};

fn rss_kib() -> Option<usize> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

fn main() {
    let lines: usize = std::env::args()
        .nth(1)
        .map_or(20_000, |arg| arg.parse().expect("line count"));
    for kind in ["ascii", "styled", "unicode"] {
        let mut input = Vec::new();
        for line in 0..lines {
            let text = match kind {
                "styled" => format!(
                    "\x1b[{}mINFO\x1b[m task={:04} file=src/terminal.rs result=complete count={line}\r\n",
                    31 + line % 6,
                    line % 1000
                ),
                "unicode" => format!(
                    "task={:04} 界面 e\u{301} 👩‍💻 保存 completed line={line}\r\n",
                    line % 1000
                ),
                _ => format!(
                    "INFO task={:04} file=src/terminal.rs result=complete count={line}\r\n",
                    line % 1000
                ),
            };
            input.extend_from_slice(text.as_bytes());
        }
        let mut terminal = Terminal::with_limits(120, 32, ScrollbackLimits::default());
        terminal.set_scrollback_compression(true);
        let feed = Instant::now();
        terminal.feed(&input);
        let feed_ms = feed.elapsed().as_secs_f64() * 1000.0;
        let expected = crc32c::crc32c(&snapshot::encode_to_vec(&terminal).unwrap());
        let raw_bytes = terminal.screen().owned_bytes();
        let raw_rss = rss_kib();
        let rows = terminal.screen().history_len();
        let start = Instant::now();
        let mut steps = Vec::new();
        loop {
            let step = Instant::now();
            let pending = terminal.compress_scrollback_step();
            steps.push(step.elapsed().as_nanos() as u64);
            if !pending {
                break;
            }
        }
        let compression_ms = start.elapsed().as_secs_f64() * 1000.0;
        let compressed_bytes = terminal.screen().owned_bytes();
        let compressed_rss = rss_kib();
        steps.sort_unstable();
        let start = Instant::now();
        black_box(terminal.screen().physical_row(0).cells[0]);
        let first_row_us = start.elapsed().as_secs_f64() * 1e6;
        let start = Instant::now();
        for row in terminal.screen().all_rows() {
            black_box(row.cells[0]);
        }
        let restore_all_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(rows, terminal.screen().history_len());
        assert_eq!(
            expected,
            crc32c::crc32c(&snapshot::encode_to_vec(&terminal).unwrap())
        );
        println!(
            "{}",
            json!({
                "case":kind, "input_bytes":input.len(), "history_rows":rows,
                "feed_ms":feed_ms, "raw_owned_bytes":raw_bytes, "compressed_owned_bytes":compressed_bytes,
                "raw_rss_kib":raw_rss, "compressed_rss_kib":compressed_rss,
                "compression_ms":compression_ms, "steps":steps.len(),
                "step_p50_us":steps[steps.len()/2] as f64 / 1000.0,
                "step_p95_us":steps[steps.len()*95/100] as f64 / 1000.0,
                "step_max_us":steps.last().unwrap().to_owned() as f64 / 1000.0,
                "first_cold_row_us":first_row_us, "restore_all_ms":restore_all_ms,
                "snapshot_crc32c":expected,
            })
        );
    }
}
