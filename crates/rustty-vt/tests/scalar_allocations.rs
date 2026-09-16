use rustty_vt::{Cell as TerminalCell, Terminal};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

fn count_allocation() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}

// SAFETY: Every operation preserves the System allocator's layout and pointer contract.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation();
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn feed_allocations(text: &str, count: usize) -> usize {
    let input = text.repeat(count);
    let mut terminal = Terminal::new(128, 20, 0);
    ALLOCATIONS.set(Some(0));
    let effects = terminal.feed(input.as_bytes());
    let allocations = ALLOCATIONS.replace(None).unwrap();
    assert!(effects.is_empty());
    assert_eq!(
        terminal.screen().row(0).cells[0].codepoint(),
        text.chars().next()
    );
    ALLOCATIONS.set(Some(0));
    let mut codepoints = 0;
    for row in terminal.screen().all_rows() {
        for col in 0..row.cells.len() {
            codepoints += terminal.screen().cell_text(row, col).chars().count();
        }
    }
    std::hint::black_box(codepoints);
    let read_allocations = ALLOCATIONS.replace(None).unwrap();
    assert_eq!(read_allocations, 0, "reading stored text must not allocate");
    allocations
}

#[test]
fn scalar_printing_does_not_allocate_per_cell() {
    #[cfg(target_pointer_width = "64")]
    assert_eq!(size_of::<TerminalCell>(), 8);

    // Initialize process-wide Unicode tables before counting terminal printing.
    Terminal::new(128, 20, 0).feed("aé界a\u{301}".as_bytes());
    let baseline = feed_allocations("", 0);
    eprintln!(
        "Cell bytes: {}, empty feed allocations: {baseline}",
        size_of::<TerminalCell>()
    );
    for text in ["a", "é", "界"] {
        let short = feed_allocations(text, 64);
        let long = feed_allocations(text, 1024);
        eprintln!("{text:?}: 64 scalars = {short} allocations, 1024 scalars = {long}");
        assert_eq!(short, baseline, "{text:?}, 64 scalars");
        assert_eq!(long, baseline, "{text:?}, 1024 scalars");
    }
    assert!(feed_allocations("a\u{301}", 64) > baseline);
}

#[test]
fn ordinary_overwrites_with_backgrounds_do_not_allocate() {
    for setup in ["", "\x1b[31;44m", "\x1b[48;2;12;34;56m\x1b[2J"] {
        let mut terminal = Terminal::new(8, 2, 0);
        terminal.feed(setup.as_bytes());
        terminal.print('é');
        let bytes = terminal.screen().owned_bytes();
        ALLOCATIONS.set(Some(0));
        for cp in ['x', 'é', ' ', 'z'].into_iter().cycle().take(1024) {
            terminal.screen_mut().cursor.col = 0;
            terminal.print(cp);
            let row = terminal.screen().row(0);
            assert_eq!(row.text(0).chars().next(), Some(cp));
        }
        assert_eq!(ALLOCATIONS.replace(None).unwrap(), 0);
        assert_eq!(terminal.screen().owned_bytes(), bytes);
    }
}

#[test]
fn grapheme_append_allocates_only_its_shared_payload() {
    let mut terminal = Terminal::new(4, 2, 0);
    terminal.feed("\x1b[?2027ha\u{301}".as_bytes());
    let snapshot = terminal.screen().snapshot_viewport();
    ALLOCATIONS.set(Some(0));
    terminal.print('\u{302}');
    assert_eq!(ALLOCATIONS.replace(None).unwrap(), 1);
    let screen = terminal.screen();
    assert_eq!(&*screen.cell_text(&screen.row(0), 0), "a\u{301}\u{302}");
    assert_eq!(&*snapshot.cell_text(&snapshot.row(0), 0), "a\u{301}");
}

#[test]
fn scrolling_exposes_initialized_page_rows_without_allocating() {
    let mut terminal = Terminal::with_limits(80, 2, Default::default());
    let capacity = usize::from(
        terminal
            .screen()
            .page_allocations()
            .next()
            .unwrap()
            .capacity
            .rows,
    );
    terminal.feed(b"first\r\n");
    let input = b"next\r\n".repeat(capacity - 2);
    ALLOCATIONS.set(Some(0));
    terminal.feed(&input);
    let allocations = ALLOCATIONS.replace(None).unwrap();
    assert_eq!(allocations, 0);
    assert_eq!(terminal.screen().page_allocations().count(), 1);
    assert_eq!(terminal.screen().history_len(), capacity - 2);
}
