//! Run with `cargo run -p rustty-render --release --example prepare_frames`.
use std::{hint::black_box, time::Instant};

fn main() {
    let mut terminal = rustty_vt::Terminal::new(120, 40, 0);
    for i in 0..35 {
        terminal.feed(
            format!("line {i:02}: command output => value != null, multilingual 水 é 👩🏽‍💻\r\n")
                .as_bytes(),
        );
    }
    let mut renderer = rustty_render::Renderer::new(rustty_font::FontConfig::default()).unwrap();
    let options = rustty_render::RenderOptions {
        size: [1200, 850],
        cursor_visible: false,
        ..Default::default()
    };
    black_box(renderer.prepare(terminal.screen(), &options).unwrap());
    let mut samples = [0.0; 5];
    for sample in &mut samples {
        let start = Instant::now();
        for _ in 0..100 {
            black_box(renderer.prepare(terminal.screen(), &options).unwrap());
        }
        *sample = start.elapsed().as_secs_f64() * 10.0;
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "Median: {:.3} ms/frame (120×40 multilingual cells, cached redraw)",
        samples[2]
    );
}
