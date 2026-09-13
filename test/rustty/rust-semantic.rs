//! Live semantic prompt state, independent of snapshot encoding.
use rustty_vt::screen::{ClickMotion, PromptRedraw, SemanticClick};
use rustty_vt::{Row, Screen, SemanticContent, Terminal};
use serde_json::{Value, json};

pub fn observe(terminal: &Terminal) -> Value {
    json!({
        "shell_redraw": match terminal.shell_prompt_redraw() {
            PromptRedraw::All => "all",
            PromptRedraw::None => "none",
            PromptRedraw::Last => "last",
        },
        "primary": screen(terminal.primary_screen()),
        "alternate": terminal.alternate_screen().map(screen),
    })
}

fn screen(screen: &Screen) -> Value {
    let (kind, relative, motion) = match screen.semantic_click() {
        SemanticClick::None => ("none", None, None),
        SemanticClick::Events { relative } => ("events", Some(relative), None),
        SemanticClick::CursorKeys { motion } => (
            "cursor_keys",
            None,
            Some(match motion {
                ClickMotion::Line => "line",
                ClickMotion::Multiple => "multiple",
                ClickMotion::ConservativeVertical => "conservative_vertical",
                ClickMotion::SmartVertical => "smart_vertical",
            }),
        ),
    };
    json!({
        "input_clears_at_eol": screen.input_clears_at_eol(),
        "click": {"kind":kind,"relative":relative,"motion":motion},
        "rows": screen.rows.iter().map(row_kind).collect::<Vec<_>>(),
        "history": screen.history.iter().map(row_kind).collect::<Vec<_>>(),
    })
}

fn row_kind(row: &Row) -> &'static str {
    match row.semantic {
        SemanticContent::Output => "none",
        SemanticContent::Prompt => "prompt",
        SemanticContent::Input => "continuation",
    }
}
