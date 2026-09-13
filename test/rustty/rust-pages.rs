//! Observe the terminal's actual page ledger without normalizing boundaries.
use rustty_vt::{Screen, Terminal};
use serde_json::{Value, json};

pub fn observe(terminal: &Terminal) -> Value {
    json!({
        "primary": screen(terminal.primary_screen()),
        "alternate": terminal.alternate_screen().map(screen),
    })
}

fn screen(screen: &Screen) -> Value {
    json!({
        "allocation_bytes": screen.storage_bytes(),
        "total_rows": screen.history.len() + screen.rows.len(),
        "pages": screen.page_allocations().collect::<Vec<_>>(),
    })
}
