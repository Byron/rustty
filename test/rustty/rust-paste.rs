use super::{ClipboardContent, Host, event, hex, unhex};
use rustty_vt::{Effect, EffectHandler, Terminal, clipboard, paste};
use serde::Deserialize;
use std::io;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    source: String,
    contents: Vec<ClipboardContent>,
    reader: bool,
    read_error: bool,
    allow_unsafe: bool,
    entropy: String,
    entropy_error: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            source: "standard".into(),
            contents: Vec::new(),
            reader: false,
            read_error: false,
            allow_unsafe: false,
            entropy: "00".into(),
            entropy_error: false,
        }
    }
}

pub fn run(
    terminal: &mut Terminal,
    host: &mut Host,
    options: &Options,
) -> Result<(), &'static str> {
    let source = match options.source.as_str() {
        "text" => paste::Source::Text,
        "standard" => paste::Source::Clipboard(clipboard::Location::Standard),
        "selection" => paste::Source::Clipboard(clipboard::Location::Selection),
        "primary" => paste::Source::Clipboard(clipboard::Location::Primary),
        _ => return Err("InvalidPasteSource"),
    };
    let contents: Vec<clipboard::Content> = options
        .contents
        .iter()
        .map(|c| {
            Ok(clipboard::Content {
                mime: unhex(&c.mime)?,
                data: unhex(&c.data)?.into(),
            })
        })
        .collect::<Result<_, &'static str>>()?;
    let mimes: Vec<_> = contents.iter().map(|c| c.mime.clone()).collect();
    let entropy = unhex(&options.entropy)?;
    if entropy.is_empty() || (!options.entropy_error && entropy.iter().all(|&b| b >= 224)) {
        return Err("InvalidTestEntropy");
    }
    let mut entropy_offset = 0;
    let mut entropy_events = Vec::new();
    let mut random = |buffer: &mut [u8]| {
        entropy_events.push(event(
            "paste_entropy",
            hex(buffer.len().to_string().as_bytes()),
        ));
        if options.entropy_error {
            return Err(io::Error::other("entropy failed"));
        }
        for byte in buffer {
            *byte = entropy[entropy_offset % entropy.len()];
            entropy_offset += 1;
        }
        Ok(())
    };
    let mut read_events = Vec::new();
    let mut read = |mime: &[u8]| {
        read_events.push(event("paste_read", hex(mime)));
        if options.read_error {
            return Err(io::Error::other("read failed"));
        }
        Ok(contents
            .iter()
            .find(|c| c.mime == mime)
            .unwrap()
            .data
            .to_vec())
    };
    let request = paste::Request {
        source,
        allow_unsafe: options.allow_unsafe,
        contents: if options.reader {
            paste::Contents::Reader {
                mimes: &mimes,
                read: &mut read,
            }
        } else {
            paste::Contents::Memory(&contents)
        },
    };
    let mut output = Vec::new();
    let result = terminal.paste(
        request,
        host.clipboard_read_enabled.then_some(&mut random),
        &mut output,
    );
    host.events.extend(entropy_events);
    host.events.extend(read_events);
    if !output.is_empty() {
        host.effect(Effect::Write(output));
    }
    let result = match result {
        Ok(true) => "true",
        Ok(false) => "false",
        Err(paste::Error::UnsafePaste) => "UnsafePaste",
        Err(paste::Error::ReadFailed(_)) => "ReadFailed",
        Err(paste::Error::EntropyUnavailable(_)) => "EntropyUnavailable",
        Err(paste::Error::WriteFailed(_)) => "WriteFailed",
    };
    host.events
        .push(event("paste_result", hex(result.as_bytes())));
    Ok(())
}
