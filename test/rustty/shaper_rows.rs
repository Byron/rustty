//! Private production run-loop checks and the opt-in shaper-rows benchmark.
//!
//! RUSTTY_SHAPER_DATA=input.vt RUSTTY_SHAPER_MODE=runs cargo +1.95.0 test \
//!   --release --locked --offline -p rustty-render --lib \
//!   prepare::shaper_rows::benchmark_shaper_rows -- --ignored --exact --nocapture
//!
//! Shape mode calls uncached shaping for every run. Rows, columns and passes
//! default to 80, 120 and 200; override RUSTTY_SHAPER_ROWS/COLS/PASSES. Feed,
//! fonts, paints and one warmup pass precede timing. Neither mode rasterizes or
//! emits quads. Rust uses JetBrains Mono with platform fallback, unlike native.
//! Run boundaries also differ: Rust performs font fallback during shaping, so
//! native and Rust run counts/timings are not interchangeable.
use super::*;
use rustty_vt::{ScrollbackLimits, Terminal};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn scratch(row: RowView<'_>, options: &RenderOptions) -> RowScratch {
    let columns = row
        .cells()
        .iter()
        .rposition(|cell| cell.bits() != 0)
        .map_or(0, |col| col + 1);
    RowScratch {
        paints: (0..columns)
            .map(|col| {
                let style = row.style(col);
                let foreground = if style.inverse {
                    resolve(style.background, options.background, options)
                } else {
                    resolve(style.foreground, options.foreground, options)
                };
                Color::rgb(foreground).opacity(if style.faint { 0.5 } else { 1.0 })
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn shaper_modes_keep_style_sprite_and_wide_boundaries_without_rasterizing() {
    for (text, expected_runs) in [
        ("ab", 1),
        ("a\x1b[1mb\x1b[0m c", 3),
        ("a━b", 3),
        ("a界b", 1),
        ("a\u{301}b", 1),
        ("a\x1b[8mhidden\x1b[0mb", 2),
    ] {
        let mut terminal = Terminal::new(20, 1, 0);
        terminal.feed(format!("\x1b[?2027h{text}").as_bytes());
        let options = RenderOptions::default();
        let row = terminal.screen().row(0);
        let mut scratch = scratch(row, &options);
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let mut frame = Frame::empty(options.size);
        assert_eq!(
            renderer
                .row_text::<FIND_RUNS>(row, &mut scratch, 0.0, &options, &mut frame)
                .unwrap(),
            expected_runs,
            "{text:?}"
        );
        for _ in 0..2 {
            assert!(
                renderer
                    .row_text::<SHAPE_RUNS>(row, &mut scratch, 0.0, &options, &mut frame)
                    .unwrap()
                    > 0
            );
        }
        assert!(frame.quads.is_empty());
        assert!(renderer.pages.is_empty());
        assert!(renderer.glyphs.is_empty());
        assert!(renderer.sprites.is_empty());
        assert!(renderer.shaped.iter().all(HashMap::is_empty));
        assert_eq!(renderer.shaped_bytes, 0);
        renderer
            .row_text::<RENDER_TEXT>(row, &mut scratch, 0.0, &options, &mut frame)
            .unwrap();
        assert!(!frame.quads.is_empty());

        if text == "a界b" {
            scratch.paints[2] = Color::rgb([255, 0, 0]);
            assert_eq!(
                renderer
                    .row_text::<FIND_RUNS>(row, &mut scratch, 0.0, &options, &mut frame)
                    .unwrap(),
                3,
                "a split-color wide cell must stay in its own clipped run"
            );
        }
    }
}

#[test]
#[ignore = "set RUSTTY_SHAPER_DATA to a VT file; see test/rustty/shaper_rows.rs"]
fn benchmark_shaper_rows() {
    fn number<T: std::str::FromStr>(name: &str, default: T) -> T
    where
        T::Err: std::fmt::Debug,
    {
        std::env::var(name).map_or(default, |value| value.parse().expect(name))
    }
    let cols: u16 = number("RUSTTY_SHAPER_COLS", 120);
    let rows: u16 = number("RUSTTY_SHAPER_ROWS", 80);
    let passes: usize = number("RUSTTY_SHAPER_PASSES", 200);
    assert!(cols != 0 && rows != 0 && passes != 0);
    let mode = std::env::var("RUSTTY_SHAPER_MODE").unwrap_or_else(|_| "shape".into());
    assert!(matches!(mode.as_str(), "runs" | "shape"));
    let data = std::fs::read(std::env::var("RUSTTY_SHAPER_DATA").expect("RUSTTY_SHAPER_DATA"))
        .expect("read shaper input");
    let mut terminal = Terminal::with_limits(
        cols,
        rows,
        ScrollbackLimits {
            bytes: Some(10_000),
            lines: None,
        },
    );
    terminal.feed(&data);
    let mut renderer = Renderer::new(FontConfig {
        size_points: 12.0,
        ..Default::default()
    })
    .unwrap();
    let metrics = renderer.metrics();
    let options = RenderOptions {
        size: [
            metrics.cell_width * u32::from(cols),
            metrics.cell_height * u32::from(rows),
        ],
        padding: [0.0; 2],
        cursor_visible: false,
        extra_cursor_visible: false,
        ..Default::default()
    };
    let (per_pass, elapsed) = match mode.as_str() {
        "runs" => measure_rows::<FIND_RUNS>(&mut renderer, terminal.screen(), &options, passes),
        "shape" => measure_rows::<SHAPE_RUNS>(&mut renderer, terminal.screen(), &options, passes),
        _ => unreachable!(),
    };
    println!(
        "{}",
        serde_json::json!({
            "mode": mode, "cols": cols, "rows": rows, "passes": passes,
            "input_bytes": data.len(), "warmup_passes": 1,
            "work_items_per_pass": per_pass, "elapsed_ns": elapsed.as_nanos(),
            "work_item": if mode == "runs" { "text run or sprite" } else { "shaped glyph or sprite" },
            "result_cache": false, "rasterization": false, "font_size_points": 12,
            "fonts": "bundled JetBrains Mono with platform fallback",
        })
    );
}

fn measure_rows<const MODE: u8>(
    renderer: &mut Renderer,
    screen: &Screen,
    options: &RenderOptions,
    passes: usize,
) -> (usize, Duration) {
    let mut scratch: Vec<_> = screen.rows().map(|row| scratch(row, options)).collect();
    let mut frame = Frame::empty(options.size);
    let mut pass = || {
        screen
            .rows()
            .zip(&mut scratch)
            .map(|(row, scratch)| {
                renderer
                    .row_text::<MODE>(row, scratch, 0.0, options, &mut frame)
                    .unwrap()
            })
            .sum::<usize>()
    };
    let per_pass = pass();
    assert!(per_pass != 0, "input must populate the visible screen");
    let start = Instant::now();
    let work: usize = (0..passes).map(|_| black_box(pass())).sum();
    let elapsed = start.elapsed();
    assert_eq!(work, per_pass * passes);
    assert_eq!(pass(), per_pass);
    assert!(frame.quads.is_empty());
    assert!(renderer.pages.is_empty());
    assert!(renderer.shaped.iter().all(HashMap::is_empty));
    (per_pass, elapsed)
}
