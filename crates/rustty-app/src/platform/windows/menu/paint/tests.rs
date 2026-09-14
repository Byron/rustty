use super::*;
use windows::Win32::{
    Foundation::COLORREF,
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
        DEFAULT_GUI_FONT, DIB_RGB_COLORS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, DeleteDC,
        DeleteObject, DrawTextW, GdiFlush, GetStockObject, HBITMAP, HGDIOBJ,
    },
};

#[test]
fn native_text_is_opaque_without_changing_pixels_outside_the_paint_rectangle() {
    let _runtime = Runtime::new().unwrap();
    let mut target = Dib::new([64, 24]);
    // Ordinary GDI RGB output has no alpha. A white desktop would shine through
    // these pixels even where native text drawing writes black foreground ink.
    for pixel in target.pixels().as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[255, 255, 255, 0]);
    }
    let rect = RECT {
        left: 4,
        top: 2,
        right: 60,
        bottom: 22,
    };
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    unsafe {
        SelectObject(target.dc, font);
        SetTextColor(target.dc, COLORREF(0));
    }
    let buffer = Buffer::new(target.dc, rect).unwrap();
    assert_eq!(unsafe { GetCurrentObject(buffer.dc, OBJ_FONT) }, font);
    let mut text: Vec<u16> = "File".encode_utf16().collect();
    let mut text_rect = rect;
    assert!(
        unsafe {
            DrawTextW(
                buffer.dc,
                &mut text,
                &mut text_rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            )
        } > 0
    );
    assert!(buffer.finish());
    unsafe { GdiFlush() }.ok().unwrap();
    let mut ink = 0;
    for (i, pixel) in target.pixels().as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = ((i % 64) as i32, (i / 64) as i32);
        if (rect.left..rect.right).contains(&x) && (rect.top..rect.bottom).contains(&y) {
            assert_eq!(pixel[3], 255, "menu pixel must hide any desktop at {x},{y}");
            ink += usize::from(pixel[..3].iter().all(|value| *value < 100));
        } else {
            assert_eq!(pixel, &[255, 255, 255, 0], "outside paint at {x},{y}");
        }
    }
    assert!(ink > 10, "native text must retain dark strokes: {ink}");
    assert_eq!(unsafe { GetCurrentObject(target.dc, OBJ_FONT) }, font);
}

struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    length: usize,
}
impl Dib {
    fn new([width, height]: [i32; 2]) -> Self {
        let dc = unsafe { CreateCompatibleDC(None) };
        assert!(!dc.is_invalid());
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .unwrap();
        let previous = unsafe { SelectObject(dc, bitmap.into()) };
        Self {
            dc,
            bitmap,
            previous,
            bits: bits.cast(),
            length: width as usize * height as usize * 4,
        }
    }

    fn pixels(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.length) }
    }
}

#[test]
#[ignore = "creates temporary native windows and reads composed desktop pixels"]
fn native_menu_colors_do_not_depend_on_the_window_behind_them() {
    use crate::platform::windows::set_transparent;
    use muda::{Menu, MenuTheme, Submenu};
    use windows::{
        Win32::{
            Graphics::{
                Dwm::DwmFlush,
                Gdi::{BLACK_BRUSH, FillRect, GetDC, HBRUSH, ReleaseDC, WHITE_BRUSH},
            },
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DrawMenuBar, GetClientRect, GetMenu,
                GetMenuItemRect, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
            },
        },
        core::w,
    };

    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
    let _runtime = Runtime::new().unwrap();
    let backdrop = Window(unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("STATIC"),
            w!("Rustty menu backdrop"),
            WS_POPUP | WS_VISIBLE,
            20,
            20,
            520,
            360,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    });
    let window = Window(unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("STATIC"),
            w!("Rustty native menu"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            40,
            40,
            360,
            240,
            Some(backdrop.0),
            None,
            None,
            None,
        )
        .unwrap()
    });
    // Only exercise native painting here. NativeMenu::new installs Muda's
    // process-global event handler, which belongs to the shortcut-routing test.
    let menu = Menu::new();
    menu.append(&Submenu::new("&File", true)).unwrap();
    unsafe { menu.init_for_hwnd(window.0.0 as isize) }.unwrap();
    attach(window.0).unwrap();

    for theme in [MenuTheme::Light, MenuTheme::Dark] {
        unsafe { menu.set_theme_for_hwnd(window.0.0 as isize, theme) }.unwrap();
        // Cover enabling and disabling alpha composition on an existing HWND.
        for transparent in [false, true, false] {
            set_transparent(window.0, transparent).unwrap();
            let mut previous = None;
            for brush in [WHITE_BRUSH, BLACK_BRUSH] {
                let mut backdrop_rect = RECT::default();
                unsafe {
                    GetClientRect(backdrop.0, &mut backdrop_rect).unwrap();
                    let dc = GetDC(Some(backdrop.0));
                    assert_ne!(
                        FillRect(dc, &backdrop_rect, HBRUSH(GetStockObject(brush).0)),
                        0
                    );
                    ReleaseDC(Some(backdrop.0), dc);
                    DrawMenuBar(window.0).unwrap();
                    GdiFlush().ok().unwrap();
                    DwmFlush().unwrap();
                }
                let mut rect = RECT::default();
                unsafe { GetMenuItemRect(Some(window.0), GetMenu(window.0), 0, &mut rect) }
                    .unwrap();
                let size = [rect.right - rect.left, rect.bottom - rect.top];
                assert!(size.into_iter().all(|value| value > 0));
                let mut capture = Dib::new(size);
                unsafe {
                    let screen = GetDC(None);
                    let result = BitBlt(
                        capture.dc,
                        0,
                        0,
                        size[0],
                        size[1],
                        Some(screen),
                        rect.left,
                        rect.top,
                        SRCCOPY,
                    );
                    ReleaseDC(None, screen);
                    result.unwrap();
                    GdiFlush().ok().unwrap();
                }
                let rgb: Vec<_> = capture
                    .pixels()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
                    .collect();
                let low = *rgb.iter().min().unwrap();
                let high = *rgb.iter().max().unwrap();
                assert!(
                    high - low > 100,
                    "menu text lost contrast: {theme:?}, transparent={transparent}"
                );
                if let Some(previous) = previous {
                    assert_eq!(
                        rgb, previous,
                        "desktop leaked into menu: {theme:?}, transparent={transparent}"
                    );
                }
                previous = Some(rgb);
            }
        }
    }
    detach(window.0);
    unsafe { menu.remove_for_hwnd(window.0.0 as isize) }.unwrap();
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            let _ = GdiFlush();
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
