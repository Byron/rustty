//! Exercise production OSC 72 state with the same host inputs as Zig.
use rustty_vt::{Terminal, dnd};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    mime: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Operation {
    action: String,
    cell_x: u32,
    cell_y: u32,
    pixel_x: i32,
    pixel_y: i32,
    operations: u8,
    mimes: Vec<String>,
    items: Vec<Item>,
}

impl Default for Operation {
    fn default() -> Self {
        Self {
            action: "observe".into(),
            cell_x: 0,
            cell_y: 0,
            pixel_x: 0,
            pixel_y: 0,
            operations: 1,
            mimes: Vec::new(),
            items: Vec::new(),
        }
    }
}

pub fn run(terminal: &mut Terminal, op: &Operation) -> Result<(Value, Vec<u8>), &'static str> {
    if !matches!(op.action.as_str(), "observe" | "move" | "leave" | "drop") {
        return Err("UnsupportedDndOperation");
    }
    if op.operations > 3 {
        return Err("InvalidDndOperations");
    }
    let Some(state) = terminal.kitty_dnd.as_mut() else {
        return Ok((Value::Null, Vec::new()));
    };
    let ev = dnd::MoveEvent {
        cell_x: op.cell_x,
        cell_y: op.cell_y,
        pixel_x: op.pixel_x,
        pixel_y: op.pixel_y,
        operations: dnd::Operations {
            copy: op.operations & 1 != 0,
            move_: op.operations & 2 != 0,
        },
    };
    let bytes = match op.action.as_str() {
        "observe" => Vec::new(),
        "leave" => state.drag_leave(),
        "move" => {
            let mimes = op
                .mimes
                .iter()
                .map(|value| super::unhex(value))
                .collect::<Result<Vec<_>, _>>()?;
            state.drag_move(ev, &mimes.iter().map(Vec::as_slice).collect::<Vec<_>>())
        }
        "drop" => {
            let items = op
                .items
                .iter()
                .map(|item| {
                    Ok(dnd::Item {
                        mime: super::unhex(&item.mime)?,
                        data: super::unhex(&item.data)?,
                    })
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            state.drag_drop(ev, &items)
        }
        _ => unreachable!(),
    };
    Ok((observe(state), bytes))
}

fn operation_name(value: dnd::Operation) -> &'static str {
    match value {
        dnd::Operation::None => "none",
        dnd::Operation::Copy => "copy",
        dnd::Operation::Move => "move",
    }
}

pub fn event_name(value: dnd::Event) -> &'static str {
    match value {
        dnd::Event::Registration => "registration",
        dnd::Event::Acceptance => "acceptance",
        dnd::Event::ConcludedNone => "concluded_none",
        dnd::Event::ConcludedCopy => "concluded_copy",
        dnd::Event::ConcludedMove => "concluded_move",
    }
}

fn event_type(value: dnd::EventType) -> &'static str {
    match value {
        dnd::EventType::Register => "register",
        dnd::EventType::Unregister => "unregister",
        dnd::EventType::Status => "status",
        dnd::EventType::Drop => "drop",
        dnd::EventType::Request => "request",
        dnd::EventType::RequestError => "request_error",
        dnd::EventType::Offer => "offer",
        dnd::EventType::Present => "present",
        dnd::EventType::StartDrag => "start_drag",
        dnd::EventType::DragEvent => "drag_event",
        dnd::EventType::DragError => "drag_error",
        dnd::EventType::RemoteData => "remote_data",
        dnd::EventType::Query => "query",
    }
}

pub fn observe(state: &dnd::State) -> Value {
    let meta = &state.chunking.metadata;
    let drop = &state.drop;
    json!({
        "chunking": {"active": state.chunking.active, "metadata": {
            "type": meta.event_type.map(event_type), "more": meta.more,
            "client_id": meta.client_id, "operation": meta.operation,
            "cell_x": meta.cell_x, "cell_y": meta.cell_y,
            "pixel_x": meta.pixel_x, "pixel_y": meta.pixel_y,
        }},
        "client_id": drop.client_id,
        "registered_mimes": super::hex(&drop.registered_mimes),
        "hovered": drop.hovered, "dropped": drop.dropped,
        "accepted": drop.accepted.map(operation_name),
        "client_accepted": state.client_accepted().map(operation_name),
        "accept_in_progress": drop.accept_in_progress,
        "accepted_mimes": super::hex(&drop.accepted_mimes),
        "offered": drop.offered.as_ref().map(|offered| offered.mimes.iter().map(|m| super::hex(m)).collect::<Vec<_>>()),
        "items": drop.items.as_ref().map(|items| items.iter().map(|item| json!({
            "mime": super::hex(&item.mime), "data": super::hex(&item.data),
        })).collect::<Vec<_>>()),
    })
}
