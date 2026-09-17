//! Keep native GDI menu painting opaque on an alpha-composited HWND.
//!
//! GDI text clears the destination alpha channel. With DWM transparency enabled,
//! black menu strokes then reveal the window behind Rustty, disappearing over a
//! white backdrop. Buffer native painting and restore alpha before presentation;
//! Windows and Muda still choose the font, colors, layout, and interaction.

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{
        BitBlt, GetBkColor, GetCurrentObject, GetTextColor, HDC, OBJ_FONT, RestoreDC, SRCCOPY,
        SaveDC, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
    },
    UI::{
        Controls::{
            BP_PAINTPARAMS, BPBF_TOPDOWNDIB, BPPF_NONCLIENT, BeginBufferedPaint, BufferedPaintInit,
            BufferedPaintSetAlpha, BufferedPaintUnInit, DRAWITEMSTRUCT, EndBufferedPaint,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GetMenuBarInfo, GetWindowRect, HMENU, MENUBARINFO, OBJID_MENU, WM_NCDESTROY,
        },
    },
};

// These are the native menu paint messages already used by Muda. Only the
// documented DRAWITEMSTRUCT and the leading UAHMENU fields need redirecting;
// leave the trailing native layout data untouched.
const WM_UAHDRAWMENU: u32 = 0x0091;
const WM_UAHDRAWMENUITEM: u32 = 0x0092;
const SUBCLASS_ID: usize = 0x5255_4d50;

#[repr(C)]
struct UahMenu {
    menu: HMENU,
    dc: HDC,
    flags: u32,
}

#[repr(C)]
struct UahItem {
    item: DRAWITEMSTRUCT,
    menu: UahMenu,
}

pub(super) struct Runtime;
impl Runtime {
    pub fn new() -> Result<Self, String> {
        unsafe { BufferedPaintInit() }.map_err(|error| format!("BufferedPaintInit: {error}"))?;
        Ok(Self)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = unsafe { BufferedPaintUnInit() };
    }
}

pub(super) fn attach(hwnd: HWND) -> Result<(), String> {
    // Install after Muda so our DC redirection wraps its native paint handler.
    unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, 0) }
        .ok()
        .map_err(|error| format!("SetWindowSubclass for menu painting: {error}"))
}

pub(super) fn detach(hwnd: HWND) {
    let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID) };
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        detach(hwnd);
    } else if lparam.0 != 0
        && matches!(message, WM_UAHDRAWMENU | WM_UAHDRAWMENUITEM)
        && let Some(result) = unsafe { paint(hwnd, message, wparam, lparam) }
    {
        return result;
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

unsafe fn paint(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let (menu, item, rect) = if message == WM_UAHDRAWMENUITEM {
        let item = lparam.0 as *mut UahItem;
        unsafe {
            (
                std::ptr::addr_of_mut!((*item).menu),
                std::ptr::addr_of_mut!((*item).item),
                (*item).item.rcItem,
            )
        }
    } else {
        (
            lparam.0 as *mut UahMenu,
            std::ptr::null_mut(),
            menu_rect(hwnd)?,
        )
    };
    let original = unsafe { (*menu).dc };
    let buffer = Buffer::new(original, rect)?;
    let item_dc = if item.is_null() {
        None
    } else {
        Some(unsafe { (*item).hDC })
    };
    // Windows owns the message storage and lends it for this synchronous call.
    // Use raw pointers rather than keeping exclusive references across callbacks.
    unsafe {
        (*menu).dc = buffer.dc;
        if !item.is_null() {
            (*item).hDC = buffer.dc;
        }
    }
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    unsafe {
        (*menu).dc = original;
        if let Some(dc) = item_dc {
            (*item).hDC = dc;
        }
    }
    buffer.finish().then_some(result)
}

fn menu_rect(hwnd: HWND) -> Option<RECT> {
    let mut info = MENUBARINFO {
        cbSize: std::mem::size_of::<MENUBARINFO>() as u32,
        ..Default::default()
    };
    let mut window = RECT::default();
    unsafe {
        GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut info).ok()?;
        GetWindowRect(hwnd, &mut window).ok()?;
    }
    Some(RECT {
        left: info.rcBar.left - window.left,
        top: info.rcBar.top - window.top - 1,
        right: info.rcBar.right - window.left,
        bottom: info.rcBar.bottom - window.top,
    })
}

struct Buffer {
    handle: isize,
    dc: HDC,
    saved: i32,
}
impl Buffer {
    fn new(target: HDC, rect: RECT) -> Option<Self> {
        let width = rect
            .right
            .checked_sub(rect.left)
            .filter(|value| *value > 0)?;
        let height = rect
            .bottom
            .checked_sub(rect.top)
            .filter(|value| *value > 0)?;
        let params = BP_PAINTPARAMS {
            cbSize: std::mem::size_of::<BP_PAINTPARAMS>() as u32,
            dwFlags: BPPF_NONCLIENT,
            ..Default::default()
        };
        let mut dc = HDC::default();
        let handle =
            unsafe { BeginBufferedPaint(target, &rect, BPBF_TOPDOWNDIB, Some(&params), &mut dc) };
        if handle == 0 {
            return None;
        }
        let buffer = Self {
            handle,
            dc,
            saved: unsafe { SaveDC(dc) },
        };
        if buffer.saved == 0 {
            return None;
        }
        unsafe {
            // A partial item repaint may retain its background. Start from the
            // existing RGB pixels, then make the entire painted rectangle opaque.
            BitBlt(
                dc,
                rect.left,
                rect.top,
                width,
                height,
                Some(target),
                rect.left,
                rect.top,
                SRCCOPY,
            )
            .ok()?;
            let font = GetCurrentObject(target, OBJ_FONT);
            if !font.is_invalid() {
                SelectObject(dc, font);
            }
            SetTextColor(dc, GetTextColor(target));
            SetBkColor(dc, GetBkColor(target));
            SetBkMode(dc, TRANSPARENT);
        }
        Some(buffer)
    }

    fn finish(mut self) -> bool {
        if unsafe { BufferedPaintSetAlpha(self.handle, None, 255) }.is_err() {
            return false;
        }
        self.restore();
        let handle = std::mem::take(&mut self.handle);
        unsafe { EndBufferedPaint(handle, true) }.is_ok()
    }

    fn restore(&mut self) {
        if self.saved != 0 {
            let _ = unsafe { RestoreDC(self.dc, std::mem::take(&mut self.saved)) };
        }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        self.restore();
        if self.handle != 0 {
            let _ = unsafe { EndBufferedPaint(self.handle, false) };
        }
    }
}

#[cfg(test)]
mod tests;
