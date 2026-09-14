//! CPU framebuffer presentation for a redirected Win32 window.
//!
//! Opaque frames use GDI directly. Translucent frames live in a passive layered
//! child, so per-pixel alpha does not replace the parent's native frame or menu.
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, OnceLock},
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush,
            GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowThreadProcessId,
            HTTRANSPARENT, IsIconic, IsWindowVisible, MA_NOACTIVATE, RegisterClassW, SW_HIDE,
            SW_SHOWNOACTIVATE, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_ERASEBKGND,
            WM_MOUSEACTIVATE, WM_NCHITTEST, WNDCLASSW, WS_CHILD, WS_DISABLED, WS_EX_LAYERED,
            WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
        },
    },
    core::w,
};
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

pub struct NativeSurface {
    presenter: Presenter,
    // Keep the parent alive until the child and its GDI resources are gone.
    _window: Arc<Window>,
}

impl NativeSurface {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let RawWindowHandle::Win32(handle) = window.window_handle().map_err(error)?.as_raw() else {
            return Err("CPU presentation requires a Win32 window".into());
        };
        let parent = HWND(handle.hwnd.get() as _);
        if unsafe { GetWindowThreadProcessId(parent, None) != GetCurrentThreadId() } {
            return Err("CPU presentation must be created on the window's event thread".into());
        }
        Ok(Self {
            presenter: Presenter::new(parent),
            _window: window,
        })
    }

    /// Present top-to-bottom physical pixels in premultiplied sRGB RGBA order.
    /// Call from the window's event thread after applying its current DPI/size.
    pub fn present(
        &mut self,
        size: [u32; 2],
        premultiplied_srgb_rgba: &[u8],
    ) -> Result<(), String> {
        self.presenter.present(size, premultiplied_srgb_rgba)
    }
}

struct Presenter {
    parent: HWND,
    child: Option<LayeredChild>,
    bitmap: Option<Bitmap>,
    layered: bool,
}

impl Presenter {
    fn new(parent: HWND) -> Self {
        Self {
            parent,
            child: None,
            bitmap: None,
            layered: false,
        }
    }

    fn present(&mut self, size: [u32; 2], rgba: &[u8]) -> Result<(), String> {
        let length = frame_length(size)?;
        if rgba.len() != length {
            return Err(format!(
                "CPU framebuffer has {} bytes; expected {length}",
                rgba.len()
            ));
        }
        if length == 0 {
            self.hide_child();
            self.bitmap = None;
            return Ok(());
        }

        let resized = self
            .bitmap
            .as_ref()
            .is_none_or(|bitmap| bitmap.size != size);
        if resized {
            // Allocate before discarding a usable old frame on allocation failure.
            self.bitmap = Some(Bitmap::new(size, length)?);
        }
        // GDI may batch reads of a DIB. Finish them before CPU writes its storage.
        unsafe { GdiFlush() }
            .ok()
            .map_err(|e| format!("GdiFlush before CPU frame: {e}"))?;
        if unsafe { !IsWindowVisible(self.parent).as_bool() || IsIconic(self.parent).as_bool() } {
            // Hidden windows may not have a drawable redirected DC. Keep the
            // frame; Winit requests another redraw when the parent is revealed.
            self.hide_child();
            self.bitmap.as_mut().unwrap().copy_rgba(rgba);
            return Ok(());
        }
        let opaque = rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255);
        if opaque {
            self.hide_child();
            let bitmap = self.bitmap.as_mut().unwrap();
            bitmap.copy_rgba(rgba);
            bitmap.blit(self.parent)?;
            return Ok(());
        }

        if self.child.is_none() {
            self.child = Some(LayeredChild::new(self.parent)?);
        }
        if !self.layered || resized {
            self.hide_child();
            // A layered child blends with its parent. Remove the previous opaque
            // client pixels so alpha reaches the desktop through Winit's DWM
            // transparent backing, instead of blending over the last frame.
            let bitmap = self.bitmap.as_mut().unwrap();
            bitmap.pixels().fill(0);
            bitmap.blit(self.parent)?;
            unsafe { GdiFlush() }
                .ok()
                .map_err(|e| format!("GdiFlush after client clear: {e}"))?;
        }
        let bitmap = self.bitmap.as_mut().unwrap();
        bitmap.copy_rgba(rgba);
        let child = self.child.as_ref().unwrap();
        let dimensions = SIZE {
            cx: size[0] as i32,
            cy: size[1] as i32,
        };
        let origin = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            // A null destination point preserves the child's parent-relative
            // origin, including when Windows moves the parent or changes DPI.
            UpdateLayeredWindow(
                child.0,
                None,
                None,
                Some(&dimensions),
                Some(bitmap.dc),
                Some(&origin),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .map_err(|e| format!("UpdateLayeredWindow for CPU frame: {e}"))?;
            if !self.layered {
                let _ = ShowWindow(child.0, SW_SHOWNOACTIVATE);
            }
        }
        self.layered = true;
        Ok(())
    }

    fn hide_child(&mut self) {
        if self.layered {
            if let Some(child) = &self.child {
                unsafe {
                    let _ = ShowWindow(child.0, SW_HIDE);
                }
            }
            self.layered = false;
        }
    }
}

fn frame_length([width, height]: [u32; 2]) -> Result<usize, String> {
    if width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err("CPU framebuffer dimensions exceed Win32 limits".into());
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
        .ok_or_else(|| "CPU framebuffer exceeds the 256 MiB image limit".into())
}

struct Bitmap {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: NonNull<u8>,
    length: usize,
    size: [u32; 2],
}

impl Bitmap {
    fn new(size: [u32; 2], length: usize) -> Result<Self, String> {
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return Err("CreateCompatibleDC failed for CPU framebuffer".into());
        }
        let mut result = Self {
            dc,
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
            bits: NonNull::dangling(),
            length,
            size,
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size[0] as i32,
                biHeight: -(size[1] as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        result.bitmap =
            unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .map_err(|e| format!("CreateDIBSection for CPU frame: {e}"))?;
        result.bits = NonNull::new(bits.cast()).ok_or("CPU framebuffer has no DIB storage")?;
        result.previous = unsafe { SelectObject(dc, result.bitmap.into()) };
        if result.previous.is_invalid() {
            return Err("SelectObject failed for CPU framebuffer".into());
        }
        Ok(result)
    }

    fn pixels(&mut self) -> &mut [u8] {
        // The selected DIB owns exactly length bytes until Bitmap::drop.
        unsafe { std::slice::from_raw_parts_mut(self.bits.as_ptr(), self.length) }
    }

    fn copy_rgba(&mut self, rgba: &[u8]) {
        for (output, input) in self
            .pixels()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(rgba.as_chunks::<4>().0)
        {
            *output = [input[2], input[1], input[0], input[3]];
        }
    }

    fn blit(&self, parent: HWND) -> Result<(), String> {
        let destination = WindowDc::new(parent)?;
        unsafe {
            BitBlt(
                destination.dc,
                0,
                0,
                self.size[0] as i32,
                self.size[1] as i32,
                Some(self.dc),
                0,
                0,
                SRCCOPY,
            )
        }
        .map_err(|e| format!("BitBlt for CPU frame: {e}"))
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = GdiFlush();
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

struct WindowDc {
    parent: HWND,
    dc: HDC,
}
impl WindowDc {
    fn new(parent: HWND) -> Result<Self, String> {
        let dc = unsafe { GetDC(Some(parent)) };
        if dc.is_invalid() {
            return Err("GetDC failed for CPU framebuffer".into());
        }
        Ok(Self { parent, dc })
    }
}
impl Drop for WindowDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(Some(self.parent), self.dc);
        }
    }
}

struct LayeredChild(HWND);
impl LayeredChild {
    fn new(parent: HWND) -> Result<Self, String> {
        static CLASS: OnceLock<Result<(), String>> = OnceLock::new();
        CLASS
            .get_or_init(|| {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(child_proc),
                    hInstance: HINSTANCE(unsafe { GetModuleHandleW(None) }.map_err(error)?.0),
                    lpszClassName: w!("Rustty.CpuFramebuffer"),
                    ..Default::default()
                };
                if unsafe { RegisterClassW(&class) } == 0 {
                    Err(format!(
                        "RegisterClassW for CPU framebuffer: {}",
                        windows::core::Error::from_thread()
                    ))
                } else {
                    Ok(())
                }
            })
            .clone()?;
        let child = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
                w!("Rustty.CpuFramebuffer"),
                w!(""),
                WS_CHILD | WS_DISABLED,
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                Some(HINSTANCE(GetModuleHandleW(None).map_err(error)?.0)),
                None,
            )
        }
        .map_err(|e| format!("CreateWindowExW for CPU layer: {e}"))?;
        Ok(Self(child))
    }
}
impl Drop for LayeredChild {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

unsafe extern "system" fn child_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // All terminal mouse, focus, IME and accessibility belong to Winit's
        // parent. The child is only an image, never an interactive control.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::{
        Dwm::{
            DWM_BB_BLURREGION, DWM_BB_ENABLE, DWM_BLURBEHIND, DwmEnableBlurBehindWindow, DwmFlush,
        },
        Gdi::{ClientToScreen, CreateRectRgn, GetPixel},
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateMenu, DestroyMenu, GetMenu, GetMenuStringW, GetWindowRect, HMENU,
        MF_BYPOSITION, MF_STRING, SendMessageW, SetMenu, WINDOW_EX_STYLE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
    };

    #[test]
    fn frame_sizes_are_checked_before_allocation() {
        assert_eq!(frame_length([0, 100]).unwrap(), 0);
        assert_eq!(frame_length([640, 480]).unwrap(), 640 * 480 * 4);
        assert!(frame_length([u32::MAX, 1]).is_err());
        assert!(frame_length([100_000, 100_000]).is_err());
    }

    #[test]
    fn hidden_surface_retains_pixels_without_a_drawable_window_dc() {
        let parent = LayeredChild(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Hidden CPU framebuffer"),
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    200,
                    120,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        let mut presenter = Presenter::new(parent.0);
        presenter
            .present([2, 1], &[12, 34, 56, 255, 32, 16, 8, 128])
            .unwrap();
        assert_eq!(
            presenter.bitmap.as_mut().unwrap().pixels(),
            [56, 34, 12, 255, 8, 16, 32, 128]
        );
        assert!(presenter.child.is_none());
        presenter.present([0, 0], &[]).unwrap();
        assert!(presenter.bitmap.is_none());
    }

    #[test]
    #[ignore = "creates temporary native windows; requires the Windows compatibility manifest"]
    fn native_framebuffer_preserves_alpha_and_passes_input_to_parent() {
        let backdrop = LayeredChild(
            unsafe {
                CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    w!("STATIC"),
                    w!("CPU framebuffer backdrop"),
                    WS_POPUP | WS_VISIBLE,
                    40,
                    40,
                    240,
                    160,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        let mut background = Bitmap::new([240, 160], 240 * 160 * 4).unwrap();
        background.copy_rgba(&[0, 0, 255, 255].repeat(240 * 160));
        background.blit(backdrop.0).unwrap();
        let parent = LayeredChild(
            unsafe {
                CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    w!("STATIC"),
                    w!("CPU framebuffer test"),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    50,
                    50,
                    200,
                    120,
                    Some(backdrop.0),
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        let menu = TestMenu {
            hwnd: parent.0,
            handle: unsafe { CreateMenu() }.unwrap(),
        };
        unsafe {
            AppendMenuW(menu.handle, MF_STRING, 1, w!("&File")).unwrap();
            SetMenu(parent.0, Some(menu.handle)).unwrap();
            let region = CreateRectRgn(0, 0, -1, -1);
            let result = DwmEnableBlurBehindWindow(
                parent.0,
                &DWM_BLURBEHIND {
                    dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION,
                    fEnable: true.into(),
                    hRgnBlur: region,
                    ..Default::default()
                },
            );
            let _ = DeleteObject(region.into());
            result.unwrap();
        }
        let mut presenter = Presenter::new(parent.0);
        assert!(presenter.present([1, 1], &[0; 3]).is_err());
        presenter.present([1, 1], &[12, 34, 56, 255]).unwrap();
        assert_eq!(
            presenter.bitmap.as_mut().unwrap().pixels(),
            [56, 34, 12, 255]
        );
        let menu_before = unsafe { GetMenu(parent.0) };
        let mut translucent = vec![0; 64 * 32 * 4];
        for (index, pixel) in translucent.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if index % 64 < 32 {
                *pixel = [32, 16, 8, 128];
            }
        }
        presenter.present([64, 32], &translucent).unwrap();
        assert_eq!(
            &presenter.bitmap.as_mut().unwrap().pixels()[..4],
            [8, 16, 32, 128]
        );
        let child = presenter.child.as_ref().unwrap().0;
        let mut rect = RECT::default();
        unsafe { GetWindowRect(child, &mut rect) }.unwrap();
        assert_eq!([rect.right - rect.left, rect.bottom - rect.top], [64, 32]);
        assert_eq!(unsafe { GetMenu(parent.0) }, menu_before);
        let mut label = [0u16; 32];
        let length = unsafe { GetMenuStringW(menu_before, 0, Some(&mut label), MF_BYPOSITION) };
        assert_eq!(
            String::from_utf16(&label[..length as usize]).unwrap(),
            "&File"
        );
        assert_eq!(
            unsafe { SendMessageW(child, WM_NCHITTEST, None, None) }.0,
            HTTRANSPARENT as isize
        );
        unsafe { GdiFlush() }.ok().unwrap();
        unsafe { DwmFlush() }.unwrap();
        let mut point = POINT::default();
        unsafe { ClientToScreen(parent.0, &mut point) }
            .ok()
            .unwrap();
        let screen = WindowDc::new(HWND::default()).unwrap();
        let blended = unsafe { GetPixel(screen.dc, point.x + 8, point.y + 8) }.0;
        let transparent = unsafe { GetPixel(screen.dc, point.x + 40, point.y + 8) }.0;
        assert_eq!(
            transparent, 0xff0000,
            "transparent pixel reveals blue backdrop"
        );
        let actual = [blended & 255, (blended >> 8) & 255, (blended >> 16) & 255];
        assert!(
            actual
                .into_iter()
                .zip([32, 16, 135])
                .all(|(a, b)| a.abs_diff(b) <= 2),
            "premultiplied pixel blends with blue backdrop: {actual:?}"
        );
        presenter.present([1, 1], &[1, 2, 3, 255]).unwrap();
        assert!(!presenter.layered);
        presenter.present([0, 0], &[]).unwrap();
        assert!(presenter.bitmap.is_none());
    }

    struct TestMenu {
        hwnd: HWND,
        handle: HMENU,
    }
    impl Drop for TestMenu {
        fn drop(&mut self) {
            unsafe {
                let _ = SetMenu(self.hwnd, None);
                let _ = DestroyMenu(self.handle);
            }
        }
    }
}
