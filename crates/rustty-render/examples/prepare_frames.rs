//! Run optimized with "--case NAME"; "--list" lists the fixed workloads.
use rustty_vt as vt;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
    time::Instant,
};

#[path = "../../../test/rustty/frame_workloads.rs"]
mod workload;

struct Allocator;
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
thread_local! {
    static COUNTS: Cell<Option<[u64; 2]>> = const { Cell::new(None) };
}
fn allocation(bytes: usize) {
    let _ = COUNTS.try_with(|counts| {
        if let Some([calls, requested]) = counts.get() {
            counts.set(Some([calls + 1, requested + bytes as u64]));
        }
    });
}
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            allocation(layout.size());
        }
        result
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc_zeroed(layout) };
        if !result.is_null() {
            allocation(layout.size());
        }
        result
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(ptr, layout, size) };
        if !result.is_null() {
            allocation(size);
        }
        result
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--list"] {
        println!("{}", serde_json::json!(workload::CASES));
        return;
    }
    let cases: Vec<_> = match args.as_slice() {
        [] => workload::CASES.to_vec(),
        [flag, case] if flag == "--case" && workload::CASES.contains(&case.as_str()) => {
            vec![case.as_str()]
        }
        _ => panic!("use --case NAME or --list"),
    };
    let mut results = Vec::new();
    for case in cases {
        let size = [120, 40];
        let mut terminal = vt::Terminal::new(size[0], size[1], 4096);
        let input = workload::setup(&mut terminal, case);
        let mut renderer = rustty_render::Renderer::new(rustty_font::FontConfig {
            families: vec!["Menlo".into()],
            size_points: 13.0,
            ..Default::default()
        })
        .unwrap();
        let options = rustty_render::RenderOptions {
            size: [1200, 850],
            cursor_visible: false,
            ..Default::default()
        };
        let mut samples = Vec::with_capacity(workload::SAMPLES);
        for frame in 0..workload::WARMUP + workload::SAMPLES {
            COUNTS.set(Some([0, 0]));
            let start = Instant::now();
            workload::advance(&mut terminal, case, &input, frame, size);
            let terminal_ns = start.elapsed().as_nanos() as u64;
            let terminal_allocations = COUNTS.replace(None).unwrap();
            COUNTS.set(Some([0, 0]));
            let start = Instant::now();
            let prepared = renderer.prepare(terminal.screen(), &options).unwrap();
            let prepare_ns = start.elapsed().as_nanos() as u64;
            let prepare_allocations = COUNTS.replace(None).unwrap();
            black_box(prepared);
            if frame >= workload::WARMUP {
                samples.push([
                    terminal_ns,
                    prepare_ns,
                    terminal_allocations[0],
                    terminal_allocations[1],
                    prepare_allocations[0],
                    prepare_allocations[1],
                ]);
            }
        }
        results.push(serde_json::json!({
            "case": case, "font": "Menlo", "font_size_points": 13,
            "size_pixels": options.size, "terminal_size": size,
            "warmup_frames": workload::WARMUP, "sample_frames": workload::SAMPLES,
            "columns": ["terminal_ns", "prepare_ns", "terminal_allocations", "terminal_requested_bytes", "prepare_allocations", "prepare_requested_bytes"],
            "statistics": {
                "terminal_ns": workload::stats(samples.iter().map(|s| s[0])),
                "prepare_ns": workload::stats(samples.iter().map(|s| s[1])),
                "terminal_allocations": workload::stats(samples.iter().map(|s| s[2])),
                "prepare_allocations": workload::stats(samples.iter().map(|s| s[4])),
            },
            "samples": samples,
        }));
    }
    println!("{}", serde_json::json!(results));
}
