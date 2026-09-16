//! Identical inputs and summaries for the renderer example and disposable app replay.
use super::vt;
use serde_json::{Value, json};

pub const CASES: [&str; 6] = [
    "cached_redraw",
    "scroll_ascii",
    "scroll_styled",
    "mixed_unicode",
    "alternate_repaint",
    "resize_reflow",
];
pub const WARMUP: usize = 50;
pub const SAMPLES: usize = 50;

pub fn setup(terminal: &mut vt::Terminal, case: &str) -> Vec<u8> {
    assert!(CASES.contains(&case), "unknown frame workload: {case}");
    terminal.feed(b"\x1b[?2027h\x1b[?25l");
    let line = match case {
        "scroll_ascii" => {
            "command output: value = 1234567890; ordinary ASCII words and punctuation\r\n"
        }
        "scroll_styled" => {
            "\x1b[1;36mcommand\x1b[0m \x1b[32mcompleted\x1b[0m: \x1b[30;43mvalue = 1234567890\x1b[0m\r\n"
        }
        _ => "command output => value != null; multilingual 水 中文 é العربية 👩🏽‍💻 ☺️\r\n",
    };
    if case == "alternate_repaint" {
        terminal.feed(b"\x1b[?1049h\x1b[?25l");
    }
    let rows = if case == "resize_reflow" {
        1000
    } else {
        usize::from(terminal.rows)
    };
    terminal.feed(line.repeat(rows).as_bytes());
    if case == "alternate_repaint" {
        format!("\x1b[H{}", line.repeat(usize::from(terminal.rows))).into_bytes()
    } else {
        line.as_bytes().to_vec()
    }
}

pub fn advance(
    terminal: &mut vt::Terminal,
    case: &str,
    input: &[u8],
    frame: usize,
    size: [u16; 2],
) {
    match case {
        "cached_redraw" => {}
        "resize_reflow" => terminal.resize(
            if frame % 2 == 0 {
                size[0].saturating_sub(20).max(2)
            } else {
                size[0]
            },
            size[1],
        ),
        _ => {
            terminal.feed(input);
        }
    }
}

pub fn stats(samples: impl Iterator<Item = u64>) -> Value {
    let mut values: Vec<_> = samples.collect();
    assert!(!values.is_empty());
    values.sort_unstable();
    let n = values.len();
    json!({
        "median": (values[(n - 1) / 2] as f64 + values[n / 2] as f64) / 2.0,
        "p95": values[(n * 95).div_ceil(100) - 1],
        "p99": values[(n * 99).div_ceil(100) - 1],
    })
}

#[cfg(test)]
#[test]
fn summaries_keep_individual_frame_tails() {
    assert_eq!(
        stats(1..=100),
        json!({"median": 50.5, "p95": 95, "p99": 99})
    );
    assert_eq!(
        stats([7].into_iter()),
        json!({"median": 7.0, "p95": 7, "p99": 7})
    );
}
