use std::{hint::black_box, time::Instant};
use tmuxx_terminal::{CaptureOptions, Terminal};

fn owned(terminal: &Terminal) -> usize {
    terminal
        .format_value("history_bytes")
        .unwrap()
        .parse()
        .unwrap()
}
fn finish(terminal: &mut Terminal) {
    terminal.request_scrollback_compression();
    let mut steps = 0;
    while terminal.compress_scrollback_step() {
        steps += 1;
        assert!(steps < 100_000);
    }
}
fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}
fn main() {
    let mut args = std::env::args().skip(1);
    let rows: usize = args.next().unwrap().parse().unwrap();
    let rounds: usize = args.next().unwrap_or("5".into()).parse().unwrap();
    assert!((100..=1_000_000).contains(&rows));
    let mut terminal = Terminal::new(120, 32, rows + 100);
    let record = "\x1b[1;32mhistorical\x1b[m\t\x1b]8;id=history;https://example.org\x07λ界 e\u{301} repeated words\x1b]8;;\x07\r\n";
    terminal.feed(record.repeat(rows).as_bytes());
    terminal.feed("\x1b[H\x1b[2Jvisible界 e\u{301}\r\nsecond\tline".as_bytes());
    let cases = [
        ("visible", CaptureOptions::default()),
        (
            "short_history",
            CaptureOptions {
                start: Some(-(rows as i32 / 2)),
                end: Some(-(rows as i32 / 2) + 3),
                ..Default::default()
            },
        ),
        (
            "history_boundary",
            CaptureOptions {
                start: Some(-2),
                end: Some(2),
                ..Default::default()
            },
        ),
        (
            "full_history",
            CaptureOptions {
                start: Some(i32::MIN),
                ..Default::default()
            },
        ),
    ];
    let expected = cases
        .iter()
        .map(|(_, options)| terminal.capture_with(*options).unwrap())
        .collect::<Vec<_>>();
    let generation = terminal.generation();
    let history = terminal.history_len();
    let mut result = Vec::new();
    for state in ["resident", "cold"] {
        for ((name, options), expected) in cases.iter().zip(&expected) {
            let mut samples = Vec::new();
            let mut before_owned = Vec::new();
            let mut after_owned = Vec::new();
            for _ in 0..rounds {
                if state == "cold" {
                    finish(&mut terminal);
                }
                before_owned.push(owned(&terminal));
                let start = Instant::now();
                let output = terminal.capture_for_owner(black_box(*options)).unwrap();
                samples.push(start.elapsed().as_nanos() as u64);
                after_owned.push(owned(&terminal));
                assert_eq!(black_box(&output), expected);
                assert_eq!(terminal.generation(), generation);
                assert_eq!(terminal.history_len(), history);
            }
            result.push(serde_json::json!({"state": state, "case": name, "bytes": expected.len(), "digest_fnv1a64": digest(expected), "samples_ns": samples, "before_owned": before_owned, "after_owned": after_owned}));
        }
    }
    println!(
        "{}",
        serde_json::json!({"rows":rows,"history_rows":history,"columns":120,"visible_rows":32,"rounds":rounds,"cases":result})
    );
}
