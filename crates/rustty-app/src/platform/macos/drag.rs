//! Borrow a drag pasteboard only inside AppKit's callback, never the user clipboard.

use super::{clipboard_mime, clipboard_type};
use crate::platform::{DragEvent, DragHandler, DragResult, MAX_DRAG_BYTES, MAX_DRAG_ITEMS};
use objc2::{AnyThread, runtime::AnyObject};
#[allow(deprecated)] // Finder still offers the legacy multi-file representation.
use objc2_app_kit::{NSFilenamesPboardType, NSPasteboard};
use objc2_foundation::{NSArray, NSString, NSURL, NSUTF8StringEncoding, NSUTF16StringEncoding};
use rustty::vt::dnd::{Item, Operations};
use std::sync::{Arc, Mutex};
use winit::{
    platform::{drag as native, macos::WindowExtMacOS},
    window::Window,
};

#[derive(Clone)]
struct Offer {
    mime: Vec<u8>,
    kind: String,
    files: bool,
}

#[derive(Default)]
struct State {
    serial: u64,
    generation: isize,
    offers: Vec<Offer>,
}

pub(super) fn install(window: &Window, handler: DragHandler) {
    let state = Arc::new(Mutex::new(State::default()));
    window.set_drag_handler(Arc::new(move |event| {
        let position = [event.position.x as f32, event.position.y as f32];
        let source_ops = Operations {
            copy: event.operations.copy,
            move_: event.operations.move_,
        };
        let board = event.source.map(|source| {
            // Winit retains this pasteboard for the duration of this callback.
            unsafe { &*source.as_ptr().cast::<NSPasteboard>() }
        });
        let incoming = (event.phase == native::DragPhase::Enter)
            .then(|| board.map(offers).unwrap_or_default());
        let (serial, generation, offered) = {
            let mut state = state.lock().unwrap();
            let offered = match event.phase {
                native::DragPhase::Enter => {
                    state.serial = state.serial.wrapping_add(1);
                    state.generation = board.map_or(-1, NSPasteboard::changeCount);
                    state.offers = incoming.unwrap();
                    state.offers.clone()
                }
                native::DragPhase::Drop => std::mem::take(&mut state.offers),
                native::DragPhase::Leave => {
                    state.offers.clear();
                    Vec::new()
                }
                native::DragPhase::Update => Vec::new(),
            };
            (state.serial, state.generation, offered)
        };
        let update = || DragEvent::Update {
            serial,
            position,
            source_ops,
        };
        let result = match event.phase {
            native::DragPhase::Enter => handler(DragEvent::Enter {
                serial,
                position,
                source_ops,
                mimes: offered.iter().map(|offer| offer.mime.clone()).collect(),
            }),
            native::DragPhase::Update => handler(update()),
            native::DragPhase::Leave => handler(DragEvent::Leave { serial }),
            native::DragPhase::Drop => {
                // The final location may differ from the last hover callback.
                let result = handler(update());
                if let DragResult::Copy(mimes) = &result
                    && source_ops.copy
                {
                    let offered: Vec<_> = offered
                        .into_iter()
                        .filter(|offer| mimes.contains(&offer.mime))
                        .collect();
                    let items = board.and_then(|board| capture(board, generation, &offered));
                    match items {
                        Some(items) => handler(DragEvent::Drop {
                            serial,
                            position,
                            source_ops,
                            items,
                        }),
                        None => {
                            handler(DragEvent::Leave { serial });
                            DragResult::Reject
                        }
                    }
                } else {
                    if result != DragResult::Default {
                        handler(DragEvent::Leave { serial });
                    }
                    result
                }
            }
        };
        match result {
            DragResult::Default => native::DragResult::Default,
            DragResult::Copy(_) if source_ops.copy => native::DragResult::Copy,
            _ => native::DragResult::Reject,
        }
    }));
    register(window, &[]);
}

pub(super) fn register(window: &Window, mimes: &[Vec<u8>]) {
    // Common types remain registered when the client uses an empty MIME list.
    let mut types = vec![
        "public.file-url".into(),
        "public.png".into(),
        "public.html".into(),
    ];
    for mime in mimes {
        if let Some(kind) = clipboard_type(mime) {
            let kind = kind.to_string();
            if !types.contains(&kind) {
                types.push(kind);
            }
        }
    }
    window.set_drag_types(types);
}

fn offers(board: &NSPasteboard) -> Vec<Offer> {
    let mut offers = Vec::new();
    for kind in board
        .types()
        .iter()
        .flat_map(|types| types.iter())
        .take(1024)
    {
        let raw = kind.to_string();
        #[allow(deprecated)]
        let files = kind.isEqualToString(unsafe { NSFilenamesPboardType });
        let mime = if files || raw == "public.file-url" {
            Some(b"text/uri-list".to_vec())
        } else if matches!(
            raw.as_str(),
            "public.utf16-plain-text" | "public.utf16-external-plain-text"
        ) {
            Some(b"text/plain".to_vec())
        } else {
            clipboard_mime(&kind)
        };
        let Some(mime) = mime else { continue };
        if !rustty::vt::dnd::valid_host_mime(&mime) {
            continue;
        }
        if !offers.iter().any(|offer: &Offer| offer.mime == mime) {
            offers.push(Offer {
                mime,
                kind: raw,
                files,
            });
            if offers.len() == MAX_DRAG_ITEMS {
                break;
            }
        }
    }
    offers
}

fn capture(board: &NSPasteboard, generation: isize, offers: &[Offer]) -> Option<Vec<Item>> {
    if generation < 0 || board.changeCount() != generation || offers.is_empty() {
        return None;
    }
    let mut items = Vec::with_capacity(offers.len());
    let mut total = 0usize;
    for offer in offers {
        let kind = NSString::from_str(&offer.kind);
        let remaining = MAX_DRAG_BYTES.checked_sub(total)?;
        let bytes = if offer.files {
            // Finder's legacy representation contains paths, not file contents.
            let paths = board.propertyListForType(&kind)?;
            let paths = paths.downcast::<NSArray<AnyObject>>().ok()?;
            let mut bytes = Vec::new();
            for path in paths {
                let path = path.downcast::<NSString>().ok()?;
                let url = NSURL::from_file_path(utf8_string(&path, remaining)?)?;
                let uri = url.absoluteString()?;
                append_uri(&mut bytes, &utf8_string(&uri, remaining)?, remaining)?;
            }
            bytes
        } else if offer.kind == "public.file-url" {
            let mut bytes = Vec::new();
            for item in board.pasteboardItems()?.iter() {
                if let Some(url) = item.stringForType(&kind) {
                    append_uri(&mut bytes, &utf8_string(&url, remaining)?, remaining)?;
                }
            }
            if bytes.is_empty() {
                return None;
            }
            bytes
        } else if offer.mime == b"text/plain" {
            // The offered native type can be UTF-16 even though its MIME is text/plain.
            let text = if matches!(
                offer.kind.as_str(),
                "public.utf16-plain-text" | "public.utf16-external-plain-text"
            ) {
                let data = board.dataForType(&kind)?;
                if data.len() > remaining {
                    return None;
                }
                NSString::initWithData_encoding(NSString::alloc(), &data, NSUTF16StringEncoding)?
            } else {
                board.stringForType(&kind)?
            };
            utf8_string(&text, remaining)?.into_bytes()
        } else {
            let data = board.dataForType(&kind)?;
            if data.len() > remaining {
                return None;
            }
            data.to_vec()
        };
        total = total.checked_add(bytes.len())?;
        if total > MAX_DRAG_BYTES {
            return None;
        }
        items.push(Item {
            mime: offer.mime.clone(),
            data: bytes,
        });
    }
    (board.changeCount() == generation).then_some(items)
}

fn utf8_string(text: &NSString, limit: usize) -> Option<String> {
    let length = text.lengthOfBytesUsingEncoding(NSUTF8StringEncoding);
    if length > limit || (length == 0 && text.length() != 0) {
        return None;
    }
    Some(text.to_string())
}

fn append_uri(bytes: &mut Vec<u8>, uri: &str, limit: usize) -> Option<()> {
    if uri.bytes().any(|b| b == b'\r' || b == b'\n')
        || bytes.len().checked_add(uri.len())?.checked_add(2)? > limit
    {
        return None;
    }
    bytes.extend_from_slice(uri.as_bytes());
    bytes.extend_from_slice(b"\r\n");
    Some(())
}
