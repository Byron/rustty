use std::time::Instant;
use rustty_vt::snapshot::{Decoder, DecodeOptions};
use serde_json::json;
fn main() {
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    let rows:usize=args[0].parse().unwrap();
    let corpus=&args[1];
    let repeats:usize=args.get(2).map(|s|s.parse().unwrap()).unwrap_or(3);
    let record=match corpus.as_str(){
        "plain"=>"Build: 日本語 😀 café output abcdefghijklmnopqrstuvwxyz\r\n",
        "styled"=>"\x1b[1;32mBuild\x1b[22;39m: 日本語 😀 café output abcdefghijklmnopqrstuvwxyz\r\n",
        "tabs"=>"\x1b[1;32mBuild\x1b[22;39m:\t日本語\t😀 café\toutput\r\n",
        _=>panic!("corpus"),
    };
    let mut owner=tmuxx_terminal::Terminal::new(128,32,rows+100);
    owner.feed(record.repeat(rows).as_bytes());
    let start=Instant::now();
    let snapshot=owner.checkpoint_snapshot();
    let checkpoint_ns=start.elapsed().as_nanos() as u64;
    let bytes=snapshot.checkpoint.as_ref().unwrap();
    let expected=serde_json::to_value(&snapshot.primary).unwrap();
    let mut samples=Vec::new();
    for _ in 0..repeats {
        let start=Instant::now();
        let mut source=bytes.as_slice();
        let mut decoder=Decoder::new(&mut source,DecodeOptions{tmux_strings:true,..Default::default()});
        let mut inner=decoder.ready().unwrap();
        decoder.set_history_cell_budget(snapshot.restored_cell_count().unwrap() as usize).unwrap();
        inner.set_tmux_history_limit(rows+100);
        let ready_ns=start.elapsed().as_nanos() as u64;
        let start=Instant::now();
        while decoder.next_history(&mut inner).unwrap().is_some(){}
        let history_ns=start.elapsed().as_nanos() as u64;
        assert!(source.is_empty());
        let start=Instant::now();
        inner.restore_view_metadata(&snapshot.primary,snapshot.alternate.as_ref()).unwrap();
        let metadata_ns=start.elapsed().as_nanos() as u64;
        assert!(serde_json::to_value(inner.primary_screen().view_metadata()).unwrap()==expected);
        let start=Instant::now();
        let full=snapshot.restore().unwrap();
        let full_restore_ns=start.elapsed().as_nanos() as u64;
        assert_eq!(full.capture_lines(Some(i32::MIN),None,false),owner.capture_lines(Some(i32::MIN),None,false));
        samples.push(json!({"ready_ns":ready_ns,"history_ns":history_ns,"metadata_ns":metadata_ns,"full_restore_ns":full_restore_ns}));
    }
    println!("{}",json!({"rows":rows,"cols":128,"screen_rows":32,"corpus":corpus,"physical_cells":snapshot.restored_cell_count().unwrap(),"checkpoint_bytes":bytes.len(),"checkpoint_ns":checkpoint_ns,"samples":samples}));
}
