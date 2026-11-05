use std::{hint::black_box, time::Instant};
use tmuxx_core::{ClientContext, Engine};

fn main() {
    let mut args = std::env::args().skip(1);
    let rows: usize = args.next().unwrap().parse().unwrap();
    let rounds: usize = args.next().unwrap_or("9".into()).parse().unwrap();
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    engine
        .options
        .insert("history-limit".into(), (rows + 100).to_string());
    let command = tmuxx_command::parse(b"new-session -d -x 120 -y 32 ''").unwrap();
    engine.execute(&command[0], &mut context).unwrap();
    let record = "historical λ text with ASCII words and \x1b[31mcolored cells\x1b[0m\r\n";
    engine.feed(0, record.repeat(rows).as_bytes());
    engine.feed(
        0,
        "\x1b[H\x1b[2JVISIBLE-Ω-42\r\nSecond\tline\r\nwide界 e\u{301}".as_bytes(),
    );
    assert!(engine.panes[&0].terminal.history_len() >= rows - 64);
    let (columns, visible_rows) = engine.panes[&0].terminal.size();
    let queries: [(&[u8], &[u8]); 3] = [
        ("#{C/r:^VISIBLE-Ω-[0-9]+$}".as_bytes(), b"1"),
        (b"#{C:Second*}", b"2"),
        (b"#{C:missing}", b"0"),
    ];
    let owned = || {
        engine.panes[&0]
            .terminal
            .format_value("history_bytes")
            .unwrap()
    };
    let before = owned();
    let sequence = engine.sequence;
    for &(query, expected) in &queries {
        assert_eq!(
            engine
                .expand(query, Some(0), Some(0), Some(0), &context)
                .unwrap(),
            expected
        );
    }
    let mut samples = Vec::new();
    for _ in 0..rounds {
        for &(query, expected) in &queries {
            let start = Instant::now();
            let output = engine
                .expand(black_box(query), Some(0), Some(0), Some(0), &context)
                .unwrap();
            let elapsed = start.elapsed().as_nanos() as u64;
            assert_eq!(black_box(output), expected);
            samples.push(elapsed);
        }
    }
    assert_eq!(owned(), before);
    assert_eq!(engine.sequence, sequence);
    println!(
        "{}",
        serde_json::json!({
            "rows": rows, "columns": columns, "visible_rows": visible_rows,
            "history_rows": engine.panes[&0].terminal.history_len(),
            "queries": queries.iter().map(|(query,_)| std::str::from_utf8(query).unwrap()).collect::<Vec<_>>(),
            "rounds": rounds, "owned_bytes": before.parse::<usize>().unwrap(),
            "samples_ns": samples,
        })
    );
}
