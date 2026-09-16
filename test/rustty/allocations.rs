//! Allocator observations kept separate from the uninstrumented timing harness.
//! Uses APIs shared with the pre-packed implementation so the same probe runs there.
use rustty_vt::{Cell as TerminalCell, ScrollbackLimits, Terminal};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

#[derive(Clone, Copy, Default, Serialize)]
struct Counts {
    allocations: usize,
    deallocations: usize,
    requested_bytes: usize,
    freed_bytes: usize,
    live_delta: i64,
    peak_delta: i64,
}

thread_local! {
    static COUNTS: Cell<Option<Counts>> = const { Cell::new(None) };
}

fn record(allocated: usize, freed: usize, allocation: bool) {
    let _ = COUNTS.try_with(|counter| {
        if let Some(mut value) = counter.get() {
            #[cfg(feature = "allocation-probe")]
            if allocation {
                rustty_vt::allocation_probe::allocation(allocated);
            }
            value.allocations += usize::from(allocation);
            value.deallocations += usize::from(freed != 0);
            value.requested_bytes += allocated;
            value.freed_bytes += freed;
            value.live_delta += allocated as i64 - freed as i64;
            value.peak_delta = value.peak_delta.max(value.live_delta);
            counter.set(Some(value));
        }
    });
}

struct CountingAllocator;

// SAFETY: All pointer/layout operations are forwarded unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record(layout.size(), 0, true);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record(layout.size(), 0, true);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(ptr, layout, size) };
        if !result.is_null() {
            record(size, layout.size(), true);
        }
        result
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(0, layout.size(), false);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn history_len(terminal: &Terminal) -> usize {
    terminal
        .screen()
        .page_allocations()
        .map(|page| usize::from(page.rows))
        .sum::<usize>()
        - usize::from(terminal.rows)
}

fn measure(name: &str, run: impl FnOnce() -> Terminal) -> Value {
    #[cfg(feature = "allocation-probe")]
    rustty_vt::allocation_probe::reset();
    COUNTS.set(Some(Counts::default()));
    let terminal = run();
    let counts = COUNTS.replace(None).unwrap();
    if size_of::<TerminalCell>() == 8 && (name.starts_with("write/") || name == "expose_rows") {
        assert_eq!(counts.allocations, 0, "{name} must not allocate");
    }
    let result = json!({
        "case": name,
        "counts": counts,
        "history_rows": history_len(&terminal),
        "history_charge": terminal.screen().history_bytes(),
        "pages": terminal.screen().page_allocations().count(),
        "native_charge": terminal.screen().storage_bytes(),
    });
    #[cfg(feature = "allocation-probe")]
    let result = {
        let mut result = result;
        result["storage_events"] =
            serde_json::to_value(rustty_vt::allocation_probe::counts()).unwrap();
        result
    };
    result
}

fn main() {
    assert!([8, 56].contains(&size_of::<TerminalCell>()));
    // Exclude process-wide initialization and generated input from every count.
    Terminal::new(128, 32, 0).feed("\x1b[?2027hé界a\u{301}👩‍💻".as_bytes());
    let mut results = vec![measure("construct/128x32", || Terminal::new(128, 32, 0))];
    for (name, pattern) in [("ascii", "a"), ("latin", "é"), ("wide", "界")] {
        let input = pattern.repeat(1024);
        let mut terminal = Terminal::new(128, 32, 0);
        results.push(measure(&format!("write/{name}"), || {
            assert!(terminal.feed(input.as_bytes()).is_empty());
            terminal
        }));
    }
    let mut terminal = Terminal::with_limits(80, 2, ScrollbackLimits::default());
    let capacity = terminal
        .screen()
        .page_allocations()
        .next()
        .unwrap()
        .capacity;
    terminal.feed(b"first\r\n");
    let input = b"next\r\n".repeat(usize::from(capacity.rows) - 2);
    results.push(measure("expose_rows", || {
        terminal.feed(&input);
        assert_eq!(terminal.screen().page_allocations().count(), 1);
        assert_eq!(history_len(&terminal), usize::from(capacity.rows) - 2);
        terminal
    }));

    let line = format!("{}\r\n", "abcdefgh".repeat(24));
    let mut terminal = Terminal::new(128, 32, 1024);
    for _ in 0..1024 {
        terminal.feed(line.as_bytes());
    }
    results.push(measure("recycle/20000_rows", || {
        for _ in 0..10_000 {
            assert!(terminal.feed(line.as_bytes()).is_empty());
            assert!(terminal.screen().page_allocations().count() <= 4);
        }
        assert!(history_len(&terminal) <= 1024);
        terminal
    }));

    let linked = format!(
        "\x1b[31;44;1m\x1b]8;id=probe;https://example.org\x07{}\x1b]8;;\x07\x1b[0m\r\n",
        "a\u{301}界".repeat(64)
    );
    for (corpus, input) in [("ascii", line), ("linked_graphemes", linked)] {
        for (policy, limit) in [
            ("unlimited", None),
            ("zero", Some(0)),
            ("512_KiB", Some(512 * 1024)),
            ("2_MiB", Some(2 * 1024 * 1024)),
        ] {
            results.push(measure(&format!("pressure/{corpus}/{policy}"), || {
                let mut terminal = Terminal::with_limits(128, 32, ScrollbackLimits::default());
                terminal.set_scrollback_memory_limit(limit);
                terminal.feed(b"\x1b[?2027h");
                for _ in 0..4096 {
                    assert!(terminal.feed(input.as_bytes()).is_empty());
                    if let Some(limit) = limit {
                        assert!(terminal.screen().history_bytes() <= limit);
                    }
                    if limit == Some(0) {
                        assert_eq!(history_len(&terminal), 0);
                    }
                }
                if limit.is_none() {
                    assert_eq!(history_len(&terminal), 8193 - 32);
                }
                terminal
            }));
        }
    }
    println!(
        "{}",
        json!({"cell_bytes": size_of::<TerminalCell>(), "cases": results})
    );
}
