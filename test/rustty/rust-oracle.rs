//! Test-only NDJSON adapter for comparisons with the original Zig terminal.
use rustty_vt::{Color, Effect, Screen, SemanticContent, Style, Terminal};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};

#[path = "rust-input.rs"]
mod input;
#[path = "rust-parser.rs"]
mod parser;

const CAPABILITIES: &[&str] = &[
    "terminal.write",
    "terminal.resize",
    "terminal.reset",
    "terminal.observe",
    "terminal.cells",
    "terminal.styles",
    "terminal.screens",
    "terminal.cursor",
    "effects.pty",
    "effects.title",
    "effects.pwd",
    "effects.bell",
    "unicode.width",
    "input.key",
    "input.mouse",
    "input.focus-paste",
    "parser.raw-events",
];
const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Request {
    id: String,
    kind: String,
    cols: u16,
    rows: u16,
    scalar: bool,
    operations: Vec<Operation>,
    codepoints: Vec<u32>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            id: "case".into(),
            kind: "terminal".into(),
            cols: 12,
            rows: 4,
            scalar: false,
            operations: Vec::new(),
            codepoints: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    op: String,
    #[serde(default)]
    data: String,
    #[serde(default)]
    cols: u16,
    #[serde(default)]
    rows: u16,
    #[serde(default)]
    input: Option<input::Event>,
}

fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    loop {
        let mut line = Vec::new();
        if (&mut input)
            .take(MAX_REQUEST_BYTES + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        if line.len() as u64 > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request exceeds 16 MiB",
            ));
        }
        let response = match serde_json::from_slice::<Request>(&line) {
            Ok(request) => match execute(&request) {
                Ok(value) => value,
                Err(error) => response(&request.id, Some(error)),
            },
            Err(_) => response("invalid", Some("InvalidRequest")),
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

fn response(id: &str, error: Option<&str>) -> Value {
    json!({"id":id,"ok":error.is_none(),"err":error,"capabilities":CAPABILITIES,
        "observations":[],"events":[],"widths":[],"parser":null})
}

fn execute(request: &Request) -> Result<Value, &'static str> {
    let mut result = response(&request.id, None);
    match request.kind.as_str() {
        "capabilities" => return Ok(result),
        "parser" => {
            result["parser"] = parser::run(&request.operations)?;
            return Ok(result);
        }
        "unicode" => {
            let mut widths = Vec::new();
            for &cp in &request.codepoints {
                let cp = char::from_u32(cp).ok_or("InvalidCodepoint")?;
                widths.push(rustty_vt::unicode::codepoint_width(cp));
            }
            result["widths"] = json!(widths);
            return Ok(result);
        }
        "terminal" | "input" => {}
        _ => return Err("UnsupportedKind"),
    }
    dimensions(request.cols, request.rows)?;
    let mut terminal = Terminal::new(request.cols, request.rows, usize::MAX);
    if request.kind == "input" {
        terminal.set_pixel_size(u32::from(request.cols) * 8, u32::from(request.rows) * 16);
    }
    let mut observations = Vec::new();
    let mut events: Vec<Value> = Vec::new();
    for operation in &request.operations {
        let effects = match operation.op.as_str() {
            "write" => {
                let bytes = unhex(&operation.data)?;
                if request.scalar {
                    bytes
                        .iter()
                        .flat_map(|byte| terminal.feed(std::slice::from_ref(byte)))
                        .collect()
                } else {
                    terminal.feed(&bytes)
                }
            }
            "resize" => {
                dimensions(operation.cols, operation.rows)?;
                terminal.resize(operation.cols, operation.rows);
                Vec::new()
            }
            "reset" => terminal.feed(b"\x1bc"),
            "observe" => {
                observations.push(observe(&terminal));
                Vec::new()
            }
            "input" => {
                let bytes =
                    input::encode(&terminal, operation.input.as_ref().ok_or("MissingInput")?)?;
                events.push(json!({"kind":"input","data":hex(&bytes)}));
                Vec::new()
            }
            _ => return Err("UnsupportedOperation"),
        };
        for effect in effects {
            let (kind, data) = match effect {
                Effect::Write(bytes) => ("write", hex(&bytes)),
                Effect::Title(text) => ("title", hex(text.as_bytes())),
                Effect::WorkingDirectory(text) => ("pwd", hex(text.as_bytes())),
                Effect::Bell => ("bell", String::new()),
                // Unknown-sequence diagnostics are not an external terminal effect.
                Effect::UnknownSequence(_) => continue,
                _ => return Err("UnsupportedEffect"),
            };
            if kind == "write"
                && let Some(last) = events.last_mut()
                && last["kind"] == "write"
            {
                let combined = format!("{}{}", last["data"].as_str().unwrap(), data);
                last["data"] = json!(combined);
                continue;
            }
            events.push(json!({"kind":kind,"data":data}));
        }
    }
    if request.kind == "terminal" {
        observations.push(observe(&terminal));
    }
    result["observations"] = json!(observations);
    result["events"] = json!(events);
    Ok(result)
}

fn dimensions(cols: u16, rows: u16) -> Result<(), &'static str> {
    if cols == 0 || rows == 0 || cols > 1024 || rows > 1024 {
        Err("InvalidDimensions")
    } else {
        Ok(())
    }
}

fn observe(terminal: &Terminal) -> Value {
    let m = &terminal.margins;
    json!({"cols":terminal.cols,"rows":terminal.rows,
        "alternate_active":terminal.is_alternate_screen(),
        "primary":screen(terminal.primary_screen()),
        "alternate":terminal.alternate_screen().map(screen),
        "margins":[m.top,m.bottom,m.left,m.right],
        "title":terminal.title,"pwd":terminal.working_directory})
}

fn screen(screen: &Screen) -> Value {
    let c = &screen.cursor;
    json!({"cursor":{"x":c.col,"y":c.row,"pending_wrap":c.pending_wrap,
        "shape":format!("{:?}",c.shape).to_lowercase(),"style":style(c.style),
        "protected":c.protected,"semantic":semantic(c.semantic)},
        "rows":screen.rows.iter().map(row).collect::<Vec<_>>(),
        "history":screen.history.iter().map(row).collect::<Vec<_>>()})
}

fn row(row: &rustty_vt::Row) -> Value {
    json!({"wrapped":row.wrapped,"cells":row.cells.iter().map(|cell| json!({
        "text":cell.text.chars().map(u32::from).collect::<Vec<_>>(),
        "width":cell.width,"spacer_head":cell.spacer_head,"style":style(cell.style),
        "hyperlink":cell.hyperlink,"protected":cell.protected,
        "semantic":semantic(cell.semantic)})).collect::<Vec<_>>()})
}

fn semantic(value: SemanticContent) -> &'static str {
    match value {
        SemanticContent::Output => "output",
        SemanticContent::Prompt => "prompt",
        SemanticContent::Input => "input",
    }
}

fn style(s: Style) -> Value {
    json!({"foreground":color(s.foreground),"background":color(s.background),
        "underline_color":color(s.underline_color),"bold":s.bold,"faint":s.faint,
        "italic":s.italic,"blink":s.blink,"inverse":s.inverse,"invisible":s.invisible,
        "strikethrough":s.strikethrough,"overline":s.overline,
        "underline":format!("{:?}",s.underline).to_lowercase()})
}

fn color(color: Color) -> Value {
    match color {
        Color::Default => json!({"kind":"default","value":[]}),
        Color::Indexed(value) => json!({"kind":"indexed","value":[value]}),
        Color::Rgb(r, g, b) => json!({"kind":"rgb","value":[r,g,b]}),
    }
}

fn unhex(text: &str) -> Result<Vec<u8>, &'static str> {
    if !text.len().is_multiple_of(2) {
        return Err("InvalidHex");
    }
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16).ok_or("InvalidHex")?;
            let lo = (pair[1] as char).to_digit(16).ok_or("InvalidHex")?;
            Ok((hi * 16 + lo) as u8)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
    output
}
