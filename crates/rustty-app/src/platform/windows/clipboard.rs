use super::wide;
use ::windows::{
    Win32::{
        Foundation::*,
        System::{DataExchange::*, Memory::*},
    },
    core::PCWSTR,
};
pub use rustty::vt::clipboard::*;
use std::{cell::RefCell, collections::HashMap, ffi::OsStr, sync::Arc};

const TEXT: u32 = 13; // CF_UNICODETEXT
const DIB: u32 = 8;
const DIBV5: u32 = 17;
const LIMIT: usize = 64 * 1024 * 1024;
const EXACT: &str = "Rustty MIME:";
struct Clipboard;
impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}
struct Allocation(HGLOBAL);
impl Drop for Allocation {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            let _ = unsafe { GlobalFree(Some(self.0)) };
        }
    }
}

fn valid(mime: &[u8]) -> Option<&str> {
    if is_text_mime(mime) {
        return std::str::from_utf8(mime).ok();
    }
    let mime = std::str::from_utf8(mime).ok()?;
    if mime.len() > 1024 || !mime.is_ascii() || mime.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let (a, b) = mime.split(';').next()?.trim().split_once('/')?;
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    };
    (token(a) && token(b)).then_some(mime)
}
fn register(name: &str) -> Option<u32> {
    let name = wide(OsStr::new(name));
    let id = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    (id != 0).then_some(id)
}
fn format(mime: &[u8]) -> Option<u32> {
    if is_text_mime(mime) {
        return Some(TEXT);
    }
    match valid(mime)? {
        "image/png" => register("PNG"),
        "text/html" => register("HTML Format"),
        name => register(name),
    }
}
fn mime(format: u32) -> Option<Vec<u8>> {
    if format == TEXT {
        return Some(b"text/plain".to_vec());
    }
    if format == DIB || format == DIBV5 {
        return Some(b"image/png".to_vec());
    }
    let mut name = [0u16; 1025];
    let len = unsafe { GetClipboardFormatNameW(format, &mut name) };
    if len <= 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&name[..len as usize]);
    let name = name.strip_prefix(EXACT).unwrap_or(&name);
    match name {
        "PNG" => Some(b"image/png".to_vec()),
        "HTML Format" => Some(b"text/html".to_vec()),
        name => valid(name.as_bytes()).map(|s| normalize(s.as_bytes())),
    }
}
fn normalize(mime: &[u8]) -> Vec<u8> {
    if is_text_mime(mime) {
        b"text/plain".to_vec()
    } else {
        mime.to_vec()
    }
}

pub(super) fn read(request: &Read, selection: &RefCell<Vec<Content>>) -> ReadResult {
    if request.location == Location::Primary {
        return ReadResult::Unsupported;
    }
    if request.location == Location::Selection {
        return selection_read(request, &selection.borrow());
    }
    if unsafe { OpenClipboard(None) }.is_err() {
        return ReadResult::Busy;
    }
    let _lock = Clipboard;
    let generation = unsafe { GetClipboardSequenceNumber() };
    let mut result = ReadSuccess::default();
    let mut formats = Vec::new();
    let mut previous = 0;
    loop {
        let next = unsafe { EnumClipboardFormats(previous) };
        if next == 0 {
            break;
        }
        if formats.len() >= 4096 {
            return ReadResult::IoError;
        }
        formats.push(next);
        if request.list
            && let Some(mime) = mime(next)
            && !result.available.contains(&mime)
        {
            result.available.push(mime);
        }
        previous = next;
    }
    let mut cache = HashMap::<Vec<u8>, Arc<[u8]>>::new();
    let mut total = 0usize;
    for requested in &request.mimes {
        if result.contents.iter().any(|c| &c.mime == requested) {
            continue;
        }
        let key = normalize(requested);
        let data = if let Some(data) = cache.get(&key) {
            data.clone()
        } else {
            let Some(native) = format(requested) else {
                continue;
            };
            let exact = valid(requested).and_then(|mime| register(&format!("{EXACT}{mime}")));
            let bytes = if let Some(exact) = exact.filter(|id| formats.contains(id)) {
                native_bytes(exact).and_then(|bytes| {
                    let length = u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?) as usize;
                    bytes
                        .get(8..8usize.checked_add(length)?)
                        .map(|s| s.to_vec())
                })
            } else if formats.contains(&native) {
                native_bytes(native).and_then(|bytes| match native {
                    TEXT => decode_text(&bytes),
                    _ if requested == b"text/html" => decode_html(&bytes),
                    _ => Some(bytes),
                })
            } else if key == b"image/png" {
                [DIBV5, DIB]
                    .into_iter()
                    .find(|id| formats.contains(id))
                    .and_then(native_bytes)
                    .and_then(|bytes| dib_png(&bytes))
            } else {
                continue;
            };
            let Some(bytes) = bytes else {
                return ReadResult::IoError;
            };
            total = total.saturating_add(bytes.len());
            if total > LIMIT {
                return ReadResult::IoError;
            }
            let data: Arc<[u8]> = bytes.into();
            cache.insert(key, data.clone());
            data
        };
        result.contents.push(Content {
            mime: requested.clone(),
            data,
        });
    }
    if generation != unsafe { GetClipboardSequenceNumber() } {
        ReadResult::Busy
    } else {
        ReadResult::Success(result)
    }
}
fn selection_read(request: &Read, contents: &[Content]) -> ReadResult {
    let mut result = ReadSuccess::default();
    if request.list {
        for content in contents {
            let mime = normalize(&content.mime);
            if !result.available.contains(&mime) {
                result.available.push(mime);
            }
        }
    }
    for mime in &request.mimes {
        if result.contents.iter().any(|c| &c.mime == mime) {
            continue;
        }
        if let Some(content) = contents
            .iter()
            .find(|c| normalize(&c.mime) == normalize(mime))
        {
            result.contents.push(Content {
                mime: mime.clone(),
                data: content.data.clone(),
            });
        }
    }
    ReadResult::Success(result)
}
fn native_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let handle = HGLOBAL(GetClipboardData(format).ok()?.0);
        let length = GlobalSize(handle);
        if length > LIMIT {
            return None;
        }
        if length == 0 {
            return Some(Vec::new());
        }
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), length).to_vec();
        let _ = GlobalUnlock(handle);
        Some(bytes)
    }
}
fn allocate(bytes: &[u8]) -> Option<Allocation> {
    unsafe {
        let allocation =
            Allocation(GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len().max(1)).ok()?);
        let pointer = GlobalLock(allocation.0);
        if pointer.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
        let _ = GlobalUnlock(allocation.0);
        Some(allocation)
    }
}
pub(super) fn write(
    request: &Write,
    selection: &RefCell<Vec<Content>>,
    owner: Option<HWND>,
) -> WriteResult {
    if request.location == Location::Primary {
        return WriteResult::Unsupported;
    }
    let mut aliases = HashMap::<Vec<u8>, Arc<[u8]>>::new();
    for content in &request.contents {
        if valid(&content.mime).is_none() {
            return WriteResult::InvalidData;
        }
        if let Some(previous) = aliases.insert(normalize(&content.mime), content.data.clone())
            && previous != content.data
        {
            return WriteResult::InvalidData;
        }
    }
    if request.location == Location::Selection {
        *selection.borrow_mut() = request.contents.clone();
        return WriteResult::Success { remember: false };
    }
    let Some(owner) = owner else {
        return WriteResult::IoError;
    };
    let mut representations = HashMap::<u32, Vec<u8>>::new();
    for content in &request.contents {
        let Some(native) = format(&content.mime) else {
            return WriteResult::InvalidData;
        };
        let Some(bytes) = (match native {
            TEXT => std::str::from_utf8(&content.data).ok().map(|text| {
                text.encode_utf16()
                    .chain(Some(0))
                    .flat_map(u16::to_le_bytes)
                    .collect()
            }),
            _ if content.mime == b"text/html" => Some(encode_html(&content.data)),
            _ => Some(content.data.to_vec()),
        }) else {
            return WriteResult::InvalidData;
        };
        representations.insert(native, bytes);
        if content.mime == b"image/png"
            && let Some(dib) = png_dib(&content.data)
        {
            representations.insert(DIBV5, dib);
        }
        // Win32 allocations may include padding. An app-private companion preserves exact
        // byte lengths while the ordinary native representation remains interoperable.
        if let Some(exact) = valid(&content.mime).and_then(|s| register(&format!("{EXACT}{s}"))) {
            let mut bytes = (content.data.len() as u64).to_le_bytes().to_vec();
            bytes.extend_from_slice(&content.data);
            representations.insert(exact, bytes);
        }
    }
    let Some(mut staged) = representations
        .into_iter()
        .map(|(id, bytes)| allocate(&bytes).map(|data| (id, data)))
        .collect::<Option<Vec<_>>>()
    else {
        return WriteResult::IoError;
    };
    if unsafe { OpenClipboard(Some(owner)) }.is_err() {
        return WriteResult::Busy;
    }
    let _lock = Clipboard;
    if unsafe { EmptyClipboard() }.is_err() {
        return WriteResult::IoError;
    }
    for (format, allocation) in &mut staged {
        if unsafe { SetClipboardData(*format, Some(HANDLE(allocation.0.0))) }.is_err() {
            return WriteResult::IoError;
        }
        allocation.0 = HGLOBAL::default(); // Ownership transferred to Windows.
    }
    WriteResult::Success { remember: false }
}
fn decode_text(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let words = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .take_while(|w| *w != 0)
        .collect::<Vec<_>>();
    Some(String::from_utf16_lossy(&words).into_bytes())
}
fn encode_html(bytes: &[u8]) -> Vec<u8> {
    let header = |start, end| {
        format!(
            "Version:1.0\r\nStartHTML:{start:010}\r\nEndHTML:{end:010}\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n"
        )
    };
    let start = header(0, 0).len();
    let mut result = header(start, start + bytes.len()).into_bytes();
    result.extend_from_slice(bytes);
    result.push(0);
    result
}
fn decode_html(bytes: &[u8]) -> Option<Vec<u8>> {
    // The ASCII header can share this slice with a truncated UTF-8 payload.
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let offset = |name: &str| {
        prefix
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|s| s.trim().parse::<usize>().ok())
    };
    Some(
        bytes
            .get(offset("StartFragment:")?..offset("EndFragment:")?)?
            .to_vec(),
    )
}
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
fn dib_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let header = word(bytes, 0)? as usize;
    if header < 40 {
        return None;
    }
    let width = word(bytes, 4)? as i32;
    let height = word(bytes, 8)? as i32;
    if width <= 0 || height == 0 || height == i32::MIN {
        return None;
    }
    let bits = u16::from_le_bytes(bytes.get(14..16)?.try_into().ok()?);
    let compression = word(bytes, 16)?;
    if !matches!(bits, 24 | 32) || !matches!(compression, 0 | 3) {
        return None;
    }
    // Support uncompressed BGR/BGRA and the standard Windows RGBA bitfields.
    if compression == 3
        && (word(bytes, 40)? != 0x00ff0000
            || word(bytes, 44)? != 0x0000ff00
            || word(bytes, 48)? != 0x000000ff)
    {
        return None;
    }
    let width = width as usize;
    let rows = height.unsigned_abs() as usize;
    let size = width.checked_mul(rows)?.checked_mul(4)?;
    if size > LIMIT {
        return None;
    }
    let stride = width.checked_mul(bits as usize)?.checked_add(31)? / 32 * 4;
    let offset = if header == 40 && compression == 3 {
        52
    } else {
        header
    };
    let pixels = bytes.get(offset..offset.checked_add(stride.checked_mul(rows)?)?)?;
    let has_alpha = bits == 32 && header >= 56 && word(bytes, 52)? == 0xff000000;
    let mut rgba = vec![0; size];
    for y in 0..rows {
        let source_y = if height > 0 { rows - 1 - y } else { y };
        for x in 0..width {
            let src = source_y * stride + x * bits as usize / 8;
            let dst = (y * width + x) * 4;
            rgba[dst..dst + 4].copy_from_slice(&[
                pixels[src + 2],
                pixels[src + 1],
                pixels[src],
                if has_alpha { pixels[src + 3] } else { 255 },
            ]);
        }
    }
    let mut result = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut result, width as u32, rows as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().ok()?.write_image_data(&rgba).ok()?;
    }
    Some(result)
}
fn png_dib(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let size = reader.output_buffer_size()?;
    if size > LIMIT {
        return None;
    }
    let mut pixels = vec![0; size];
    let info = reader.next_frame(&mut pixels).ok()?;
    let size = (info.width as usize)
        .checked_mul(info.height as usize)?
        .checked_mul(4)?;
    if size > LIMIT || info.width > i32::MAX as u32 || info.height > i32::MAX as u32 {
        return None;
    }
    let mut result = vec![0; 124 + size];
    for (offset, value) in [
        (0, 124u32),
        (4, info.width),
        (8, (0i32.checked_sub(info.height as i32)?) as u32),
        (16, 3),
        (20, size as u32),
        (40, 0xff0000),
        (44, 0xff00),
        (48, 0xff),
        (52, 0xff000000),
        (56, 0x73524742),
    ] {
        result[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    result[12..14].copy_from_slice(&1u16.to_le_bytes());
    result[14..16].copy_from_slice(&32u16.to_le_bytes());
    let channels = info.color_type.samples();
    for (source, dest) in pixels[..info.buffer_size()]
        .chunks_exact(channels)
        .zip(result[124..].as_chunks_mut::<4>().0.iter_mut())
    {
        let (r, g, b, a) = match info.color_type {
            png::ColorType::Rgba => (source[0], source[1], source[2], source[3]),
            png::ColorType::Rgb => (source[0], source[1], source[2], 255),
            png::ColorType::Grayscale => (source[0], source[0], source[0], 255),
            png::ColorType::GrayscaleAlpha => (source[0], source[0], source[0], source[1]),
            _ => return None,
        };
        dest.copy_from_slice(&[b, g, r, a]);
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_offsets_count_utf8_bytes() {
        let html = "<b>界</b>".as_bytes();
        assert_eq!(decode_html(&encode_html(html)).unwrap(), html);
    }
    #[test]
    fn invalid_mime_cannot_be_registered() {
        assert!(valid(b"text/plain\r\nother").is_none());
        assert!(valid(b"application/x-rustty-test").is_some());
    }
    #[test]
    fn selection_preserves_binary_aliases_and_rejects_conflicting_text() {
        let selection = RefCell::default();
        let request = Write::osc52(
            Location::Selection,
            vec![Content {
                mime: b"UTF8_STRING".to_vec(),
                data: Arc::from(&b"hi\0there"[..]),
            }],
        );
        assert!(matches!(
            write(&request, &selection, None),
            WriteResult::Success { .. }
        ));
        let mut request = Read::osc52(Location::Selection, Terminator::St);
        request.list = true;
        let ReadResult::Success(result) = read(&request, &selection) else {
            panic!("selection read failed")
        };
        assert_eq!(result.available, [b"text/plain".to_vec()]);
        assert_eq!(&*result.contents[0].data, b"hi\0there");
        let conflict = Write::osc52(
            Location::Selection,
            vec![
                Content {
                    mime: b"text/plain".to_vec(),
                    data: Arc::from(&b"a"[..]),
                },
                Content {
                    mime: b"TEXT".to_vec(),
                    data: Arc::from(&b"b"[..]),
                },
            ],
        );
        assert_eq!(write(&conflict, &selection, None), WriteResult::InvalidData);
        assert_eq!(&*selection.borrow()[0].data, b"hi\0there");
    }
    #[test]
    fn png_dib_roundtrip_preserves_color_and_alpha() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255, 0, 128, 200, 0, 255, 16, 0])
                .unwrap();
        }
        let dib = png_dib(&png).unwrap();
        let encoded = dib_png(&dib).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(encoded))
            .read_info()
            .unwrap();
        let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut rgba).unwrap();
        assert_eq!(rgba, [255, 0, 128, 200, 0, 255, 16, 0]);
    }
}
