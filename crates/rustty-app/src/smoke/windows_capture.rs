//! Capture the actual composited window, including its native frame and menu.
use std::{error::Error, fs::File, io, io::BufWriter, path::Path};
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::{
        Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmFlush, DwmGetWindowAttribute},
        Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP,
            HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
        },
    },
    UI::WindowsAndMessaging::{
        GetForegroundWindow, GetSystemMetrics, GetWindowRect, IsIconic, IsWindowVisible,
        SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    },
};
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

pub(super) fn capture(window: &Window, path: &Path) -> Result<(), Box<dyn Error>> {
    let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
        return Err(io::Error::other("native capture requires a Win32 window").into());
    };
    let hwnd = HWND(handle.hwnd.get() as _);
    ensure_foreground(hwnd)?;
    unsafe { DwmFlush()? };

    let mut bounds = RECT::default();
    if unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut bounds as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .is_err()
    {
        unsafe { GetWindowRect(hwnd, &mut bounds)? };
    }
    let width = bounds.right.checked_sub(bounds.left).filter(|v| *v > 0);
    let height = bounds.bottom.checked_sub(bounds.top).filter(|v| *v > 0);
    let (Some(width), Some(height)) = (width, height) else {
        return Err(io::Error::other("native capture has invalid window bounds").into());
    };
    let length = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= 256 * 1024 * 1024)
        .ok_or_else(|| io::Error::other("native capture exceeds the image size limit"))?;
    let (left, top, screen_width, screen_height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if bounds.left < left
        || bounds.top < top
        || i64::from(bounds.right) > i64::from(left) + i64::from(screen_width)
        || i64::from(bounds.bottom) > i64::from(top) + i64::from(screen_height)
    {
        return Err(io::Error::other("native capture window is partly off screen").into());
    }

    let mut capture = CaptureDc {
        screen: unsafe { GetDC(None) },
        ..Default::default()
    };
    if capture.screen.is_invalid() {
        return Err(io::Error::other("GetDC failed for native capture").into());
    }
    capture.memory = unsafe { CreateCompatibleDC(Some(capture.screen)) };
    if capture.memory.is_invalid() {
        return Err(io::Error::other("CreateCompatibleDC failed for native capture").into());
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative height stores the screen rows in top-to-bottom order.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    capture.bitmap = unsafe {
        CreateDIBSection(
            Some(capture.screen),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )?
    };
    if bits.is_null() {
        return Err(io::Error::other("native capture bitmap has no pixel storage").into());
    }
    capture.previous = unsafe { SelectObject(capture.memory, capture.bitmap.into()) };
    if capture.previous.is_invalid() {
        return Err(io::Error::other("SelectObject failed for native capture").into());
    }
    // Read the desktop pixels. PrintWindow would repaint the menu offscreen and
    // could hide a defect in the HWND's actual DWM composition.
    unsafe {
        BitBlt(
            capture.memory,
            0,
            0,
            width,
            height,
            Some(capture.screen),
            bounds.left,
            bounds.top,
            SRCCOPY | CAPTUREBLT,
        )?;
        GdiFlush().ok()?;
    }
    ensure_foreground(hwnd)?;
    // The DIB is BGRA; screen alpha is unspecified after GDI composition.
    let mut pixels = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), length) }.to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    drop(capture);

    let mut encoder = png::Encoder::new(
        BufWriter::new(File::create(path)?),
        width as u32,
        height as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    Ok(())
}

fn ensure_foreground(hwnd: HWND) -> io::Result<()> {
    if unsafe {
        GetForegroundWindow() != hwnd
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
    } {
        return Err(io::Error::other(
            "native capture requires the visible foreground window",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct CaptureDc {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Drop for CaptureDc {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.memory.is_invalid() {
                let _ = DeleteDC(self.memory);
            }
            if !self.screen.is_invalid() {
                ReleaseDC(None, self.screen);
            }
        }
    }
}
