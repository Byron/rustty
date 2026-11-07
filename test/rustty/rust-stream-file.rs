//! Run: cargo run --release -p rustty-vt --example stream-file -- CONFIG.json ITERATIONS
#[path = "stream-file-bench.rs"]
pub mod workload;

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args.len(), 3, "usage: stream-file CONFIG.json ITERATIONS");
    let (config, bytes) = workload::load(std::path::Path::new(&args[1]));
    let iterations = args[2]
        .to_str()
        .unwrap()
        .parse()
        .expect("positive iteration count");
    println!(
        "{}",
        serde_json::to_string(&workload::run(&config, &bytes, iterations)).unwrap()
    );
}
