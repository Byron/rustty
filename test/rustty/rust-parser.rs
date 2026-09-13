//! Raw parser observations. OSC command validation belongs to terminal tests.
use rustty_parser::{Event, Parser};
use serde_json::{Value, json};

pub fn run(operations: &[super::Operation]) -> Result<Value, &'static str> {
    let mut parser = Parser::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    for operation in operations {
        match operation.op.as_str() {
            "reset" => parser.reset(),
            "observe" => states.push(state(&parser, events.len())),
            "write" => parser.advance(&super::unhex(&operation.data)?, |event| {
                events.push(observe(event))
            }),
            _ => return Err("UnsupportedOperation"),
        }
    }
    states.push(state(&parser, events.len()));
    Ok(json!({"events":events,"states":states}))
}

fn state(parser: &Parser, events: usize) -> Value {
    const NAMES: &[&str] = &[
        "ground",
        "escape",
        "escape_intermediate",
        "csi_entry",
        "csi_intermediate",
        "csi_param",
        "csi_ignore",
        "dcs_entry",
        "dcs_param",
        "dcs_intermediate",
        "dcs_passthrough",
        "dcs_ignore",
        "osc_string",
        "sos_pm_apc_string",
    ];
    json!({"state":NAMES[parser.state() as usize],"ground":parser.is_ground(),"events":events})
}

fn observe(event: Event<'_>) -> Value {
    let mut value = json!({"kind":"","codepoint":null,"byte":null,"intermediates":"",
        "params":[],"colon_separators":0,"data":"","bell":false});
    let kind = match event {
        Event::Print(cp) => {
            value["codepoint"] = json!(cp as u32);
            "print"
        }
        Event::Execute(byte) => {
            value["byte"] = json!(byte);
            "execute"
        }
        Event::Esc {
            intermediates,
            final_byte,
        } => {
            value["intermediates"] = json!(super::hex(intermediates));
            value["byte"] = json!(final_byte);
            "esc"
        }
        Event::Csi {
            intermediates,
            params,
            colon_separators,
            final_byte,
        } => {
            value["intermediates"] = json!(super::hex(intermediates));
            value["params"] = json!(params);
            value["colon_separators"] = json!(colon_separators);
            value["byte"] = json!(final_byte);
            "csi"
        }
        Event::DcsHook {
            intermediates,
            params,
            final_byte,
        } => {
            value["intermediates"] = json!(super::hex(intermediates));
            value["params"] = json!(params);
            value["byte"] = json!(final_byte);
            "dcs_hook"
        }
        Event::DcsPut(byte) => {
            value["byte"] = json!(byte);
            "dcs_put"
        }
        Event::ApcPut(byte) => {
            value["byte"] = json!(byte);
            "apc_put"
        }
        Event::DcsUnhook => "dcs_unhook",
        Event::ApcStart => "apc_start",
        Event::ApcEnd => "apc_end",
        Event::Osc {
            data,
            terminated_by_bell,
        } => {
            value["data"] = json!(super::hex(data));
            value["bell"] = json!(terminated_by_bell);
            "osc"
        }
        Event::OscOverflow => "osc_overflow",
    };
    value["kind"] = json!(kind);
    value
}
