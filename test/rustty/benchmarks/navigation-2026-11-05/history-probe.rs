//! Isolated large-history storage/access probe; no PTY, renderer, or GUI.
use rustty_vt::{GridPoint, ScrollbackLimits, Selection, Terminal, search::{TerminalSearch, Status}, snapshot};
use serde_json::json;
use std::{hint::black_box, time::Instant};

fn compress(t: &mut Terminal) -> (usize, u128, u128) {
    t.request_scrollback_compression();
    let mut steps = 0;
    let mut max = 0;
    let all = Instant::now();
    loop {
        let start = Instant::now();
        let more = t.compress_scrollback_step();
        max = max.max(start.elapsed().as_nanos());
        steps += 1;
        if !more { break; }
        assert!(steps < 1_000_000);
    }
    (steps, all.elapsed().as_nanos(), max)
}

fn measure(base: &Terminal, case: &str, op: impl Fn(&mut Terminal) -> usize) {
    let mut t = base.clone();
    let before = t.screen().owned_bytes();
    let start = Instant::now();
    let result = black_box(op(&mut t));
    let cold_ns = start.elapsed().as_nanos();
    let after = t.screen().owned_bytes();
    let start = Instant::now();
    let again = black_box(op(&mut t));
    let warm_ns = start.elapsed().as_nanos();
    let (steps, recompress_ns, step_max_ns) = compress(&mut t);
    println!("{}", json!({"case":case,"cold_ns":cold_ns,"warm_ns":warm_ns,"result":result,"again":again,"before_owned":before,"after_owned":after,"recompressed_owned":t.screen().owned_bytes(),"recompress_ns":recompress_ns,"recompress_steps":steps,"recompress_step_max_ns":step_max_ns}));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let rows: usize = args.next().unwrap_or("20000".into()).parse().unwrap();
    let kind = args.next().unwrap_or("ascii".into());
    let only = args.next();
    let mut input = Vec::new();
    for i in 0..rows {
        let line = if kind == "unicode" {
            format!("\x1b[{}mline={i:06} 界面 e\u{301} 👩‍💻 保存 completed\x1b[m\r\n",31+i%6)
        } else {
            format!("INFO line={i:06} task={:04} file=src/terminal.rs result=complete\r\n",i%1000)
        };
        input.extend_from_slice(line.as_bytes());
    }
    let mut base = Terminal::with_limits(120,32,ScrollbackLimits::default());
    base.set_scrollback_compression(true);
    let start = Instant::now();
    base.feed(&input);
    let feed_ns = start.elapsed().as_nanos();
    let raw_owned = base.screen().owned_bytes();
    let history = base.screen().history_len();
    let last = history + base.screen().height() - 2;
    let id = base.screen().physical_row(last).id;
    let (steps, compress_ns, step_max_ns) = compress(&mut base);
    println!("{}",json!({"case":"setup","rows":rows,"kind":kind,"input_bytes":input.len(),"history_rows":history,"feed_ns":feed_ns,"raw_owned":raw_owned,"cold_owned":base.screen().owned_bytes(),"pages":base.screen().page_allocations().count(),"compress_ns":compress_ns,"compress_steps":steps,"compress_step_max_ns":step_max_ns}));
    macro_rules! run { ($name:literal,$op:expr) => { if only.as_ref().is_none_or(|s| s==$name) { measure(&base,$name,$op); } }; }
    run!("point_tail", |t| t.screen().point(last,0).unwrap().row as usize);
    run!("track_tail", |t| { let p=t.screen_mut().track(GridPoint{row:id,col:0}); let r=t.screen().resolve(p).unwrap().row; t.screen_mut().untrack(p); r as usize });
    run!("viewport", |t| t.screen().viewport().map(|r| black_box(r.cells[0]).bits() as usize).fold(0,usize::wrapping_add));
    run!("snapshot_viewport", |t| t.screen().snapshot_viewport().all_rows().count());
    run!("snapshot_selection_tail", |t| {t.screen_mut().selection=Some(Selection{start:GridPoint{row:id,col:0},end:GridPoint{row:id,col:10},rectangular:false});t.screen().snapshot_viewport().all_rows().count()});
    run!("selection_text_tail", |t| {t.screen_mut().selection=Some(Selection{start:GridPoint{row:id,col:0},end:GridPoint{row:id,col:10},rectangular:false});t.screen().selection_text().unwrap().len()});
    run!("search_first_feed", |t| {let mut search=TerminalSearch::new(b"line=000123");search.feed(t,true);search.total_matches()});
    run!("search_run_rare", |t| {let mut search=TerminalSearch::new(b"line=000123");search.run(t);assert_eq!(search.status(),Status::Complete);search.total_matches()});
    run!("snapshot_full", |t| snapshot::encode_to_vec(t).unwrap().len());
    run!("reflow_roundtrip", |t| {t.resize(60,32);t.resize(120,32);t.screen().history_len()});
}
