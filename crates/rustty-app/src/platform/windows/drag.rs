//! Incoming IDataObject reads, kept separate from the global clipboard.

use super::clipboard;
use crate::platform::{DragEvent, DragHandler, DragResult, MAX_DRAG_BYTES, MAX_DRAG_ITEMS};
use rustty::vt::dnd::{Item, Operations};
use std::sync::{Arc, Mutex};
use windows::{
    Win32::{
        System::{
            Com::{
                CoTaskMemFree, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM,
                TYMED_HGLOBAL,
            },
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::ReleaseStgMedium,
        },
        UI::Shell::{DragQueryFileW, HDROP, UrlCreateFromPathW},
    },
    core::{Interface, PCWSTR, PWSTR},
};
use winit::{
    platform::{drag as native, windows::WindowExtWindows},
    window::Window,
};

#[derive(Clone)]
struct Offer {
    mime: Vec<u8>,
    format: u16,
}
#[derive(Default)]
struct State {
    serial: u64,
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
        let pointer = event
            .source
            .map(|source| source.as_ptr())
            .unwrap_or(std::ptr::null_mut());
        // The native callback owns this borrowed interface. Do not retain it.
        let object = unsafe { IDataObject::from_raw_borrowed(&pointer) };
        let incoming = (event.phase == native::DragPhase::Enter)
            .then(|| object.map(offers).unwrap_or_default());
        let (serial, offered) = {
            let mut state = state.lock().unwrap();
            let offered = match event.phase {
                native::DragPhase::Enter => {
                    state.serial = state.serial.wrapping_add(1);
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
            (state.serial, offered)
        };
        let result = match event.phase {
            native::DragPhase::Enter => handler(DragEvent::Enter {
                serial,
                position,
                source_ops,
                mimes: offered.iter().map(|offer| offer.mime.clone()).collect(),
            }),
            native::DragPhase::Update => handler(DragEvent::Update {
                serial,
                position,
                source_ops,
            }),
            native::DragPhase::Leave => handler(DragEvent::Leave { serial }),
            native::DragPhase::Drop => {
                let result = handler(DragEvent::Update {
                    serial,
                    position,
                    source_ops,
                });
                if let DragResult::Copy(mimes) = &result
                    && source_ops.copy
                {
                    let offered: Vec<_> = offered
                        .into_iter()
                        .filter(|offer| mimes.contains(&offer.mime))
                        .collect();
                    let items = object.and_then(|object| capture(object, &offered));
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
}

fn offers(object: &IDataObject) -> Vec<Offer> {
    let Ok(formats) = (unsafe { object.EnumFormatEtc(DATADIR_GET.0 as u32) }) else {
        return Vec::new();
    };
    let mut offers = Vec::new();
    // Enumeration is metadata only. Bound hostile providers that never finish.
    for _ in 0..1024 {
        let mut row = [FORMATETC::default()];
        let mut fetched = 0;
        if unsafe { formats.Next(&mut row, Some(&mut fetched)) }.is_err() || fetched == 0 {
            break;
        }
        let format = row[0];
        if !format.ptd.is_null() {
            unsafe { CoTaskMemFree(Some(format.ptd.cast())) };
        }
        if format.tymed & TYMED_HGLOBAL.0 as u32 == 0 || format.dwAspect != DVASPECT_CONTENT.0 {
            continue;
        }
        let mime = if format.cfFormat == 15 {
            Some(b"text/uri-list".to_vec())
        } else {
            clipboard::mime(u32::from(format.cfFormat))
        };
        let Some(mime) = mime else { continue };
        if !rustty::vt::dnd::valid_host_mime(&mime) {
            continue;
        }
        if !offers.iter().any(|offer: &Offer| offer.mime == mime) {
            offers.push(Offer {
                mime,
                format: format.cfFormat,
            });
            if offers.len() == MAX_DRAG_ITEMS {
                break;
            }
        }
    }
    offers
}

struct Medium(STGMEDIUM);
impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

fn capture(object: &IDataObject, offers: &[Offer]) -> Option<Vec<Item>> {
    if offers.is_empty() {
        return None;
    }
    let mut items = Vec::with_capacity(offers.len());
    let mut raw_total = 0usize;
    let mut total = 0usize;
    for offer in offers {
        let format = FORMATETC {
            cfFormat: offer.format,
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        let medium = Medium(unsafe { object.GetData(&format) }.ok()?);
        if medium.0.tymed != TYMED_HGLOBAL.0 as u32 {
            return None;
        }
        let handle = unsafe { medium.0.u.hGlobal };
        if handle.0.is_null() {
            return None;
        }
        let length = unsafe { GlobalSize(handle) };
        raw_total = raw_total.checked_add(length)?;
        if raw_total > MAX_DRAG_BYTES {
            return None;
        }
        let remaining = MAX_DRAG_BYTES.checked_sub(total)?;
        let data = if offer.format == 15 {
            file_uris(HDROP(handle.0), remaining)?
        } else if length == 0 {
            clipboard::drag_bytes(u32::from(offer.format), &offer.mime, &[], remaining)?
        } else {
            let pointer = unsafe { GlobalLock(handle) };
            if pointer.is_null() {
                return None;
            }
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            let data =
                clipboard::drag_bytes(u32::from(offer.format), &offer.mime, bytes, remaining);
            let _ = unsafe { GlobalUnlock(handle) };
            data?
        };
        total = total.checked_add(data.len())?;
        if total > MAX_DRAG_BYTES {
            return None;
        }
        items.push(Item {
            mime: offer.mime.clone(),
            data,
        });
    }
    Some(items)
}

fn file_uris(drop: HDROP, limit: usize) -> Option<Vec<u8>> {
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    let mut bytes = Vec::new();
    for index in 0..count {
        let length = unsafe { DragQueryFileW(drop, index, None) } as usize;
        if length > limit / 2 {
            return None;
        }
        let mut path = vec![0; length.checked_add(1)?];
        if unsafe { DragQueryFileW(drop, index, Some(&mut path)) } as usize != length {
            return None;
        }
        let capacity = length.checked_mul(3)?.checked_add(32)?;
        let mut url = vec![0; capacity];
        let mut size = u32::try_from(capacity).ok()?;
        unsafe { UrlCreateFromPathW(PCWSTR(path.as_ptr()), PWSTR(url.as_mut_ptr()), &mut size, 0) }
            .ok()?;
        let uri = String::from_utf16(url.get(..size as usize)?).ok()?;
        if bytes.len().checked_add(uri.len())?.checked_add(2)? > limit || uri.contains(['\r', '\n'])
        {
            return None;
        }
        bytes.extend_from_slice(uri.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        mem::ManuallyDrop,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use windows::{
        Win32::{
            Foundation::{E_NOTIMPL, GlobalFree, HGLOBAL},
            System::{
                Com::*,
                DataExchange::RegisterClipboardFormatW,
                Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc},
            },
            UI::Shell::SHCreateStdEnumFmtEtc,
        },
        core::{BOOL, HRESULT, IUnknown, IUnknownImpl, Ref, Result, implement},
    };

    #[implement(IDataObject)]
    struct Source {
        contents: Vec<(u16, Vec<u8>)>,
        reads: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        allocations: Mutex<Vec<usize>>,
    }
    impl Drop for Source {
        fn drop(&mut self) {
            for pointer in self.allocations.get_mut().unwrap().drain(..) {
                let _ = unsafe { GlobalFree(Some(HGLOBAL(pointer as *mut _))) };
            }
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[allow(non_snake_case)]
    impl IDataObject_Impl for Source_Impl {
        fn GetData(&self, format: *const FORMATETC) -> Result<STGMEDIUM> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let format = unsafe { &*format };
            let bytes = &self
                .contents
                .iter()
                .find(|(id, _)| *id == format.cfFormat)
                .ok_or_else(|| windows::core::Error::from(E_NOTIMPL))?
                .1;
            let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len().max(1)) }?;
            let pointer = unsafe { GlobalLock(handle) };
            assert!(!pointer.is_null());
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len()) };
            let _ = unsafe { GlobalUnlock(handle) };
            self.allocations.lock().unwrap().push(handle.0 as usize);
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: handle },
                pUnkForRelease: ManuallyDrop::new(Some(
                    self.to_interface::<IDataObject>().cast::<IUnknown>()?,
                )),
            })
        }
        fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn QueryGetData(&self, _: *const FORMATETC) -> HRESULT {
            E_NOTIMPL
        }
        fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> HRESULT {
            E_NOTIMPL
        }
        fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn EnumFormatEtc(&self, _: u32) -> Result<IEnumFORMATETC> {
            let formats: Vec<_> = self
                .contents
                .iter()
                .map(|(format, _)| FORMATETC {
                    cfFormat: *format,
                    dwAspect: DVASPECT_CONTENT.0,
                    lindex: -1,
                    tymed: TYMED_HGLOBAL.0 as u32,
                    ..Default::default()
                })
                .collect();
            unsafe { SHCreateStdEnumFmtEtc(&formats) }
        }
        fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
            Err(E_NOTIMPL.into())
        }
        fn DUnadvise(&self, _: u32) -> Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
            Err(E_NOTIMPL.into())
        }
    }

    #[test]
    fn drag_data_is_deferred_bounded_and_releases_com_media() {
        let reads = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let name: Vec<u16> = "Rustty MIME:application/x-rustty-drag"
            .encode_utf16()
            .chain([0])
            .collect();
        let format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) } as u16;
        assert_ne!(format, 0);
        let mut raw = 4u64.to_le_bytes().to_vec();
        raw.extend_from_slice(&[1, 2, 0, 3]);
        let text: Vec<u8> = "🦀\ntext\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let object: IDataObject = Source {
            contents: vec![(13, text), (format, raw)],
            reads: reads.clone(),
            dropped: dropped.clone(),
            allocations: Mutex::default(),
        }
        .into();
        let offer = offers(&object);
        assert_eq!(offer.len(), 2);
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        let items = capture(&object, &offer).unwrap();
        assert_eq!(items[0].data, "🦀\ntext".as_bytes());
        assert_eq!(items[1].data, [1, 2, 0, 3]);
        assert_eq!(reads.load(Ordering::Relaxed), 2);
        drop(object);
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            1,
            "ReleaseStgMedium must release pUnkForRelease"
        );

        let object: IDataObject = Source {
            contents: vec![(format, vec![0; MAX_DRAG_BYTES + 1])],
            reads,
            dropped: dropped.clone(),
            allocations: Mutex::default(),
        }
        .into();
        assert!(capture(&object, &offers(&object)).is_none());
        drop(object);
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            2,
            "oversize rejection must release the acquired medium"
        );

        let name: Vec<u16> = "Rustty MIME:text/plain;charset=utf-8"
            .encode_utf16()
            .chain([0])
            .collect();
        let format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) } as u16;
        let mut bytes = 4u64.to_le_bytes().to_vec();
        bytes.extend_from_slice("🦀".as_bytes());
        let object: IDataObject = Source {
            contents: vec![(format, bytes)],
            reads: Arc::default(),
            dropped,
            allocations: Mutex::default(),
        }
        .into();
        let items = capture(&object, &offers(&object)).unwrap();
        assert_eq!(items[0].mime, b"text/plain");
        assert_eq!(items[0].data, "🦀".as_bytes());
    }
}
