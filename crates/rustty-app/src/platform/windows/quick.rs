use super::err;
use ::windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    UI::{
        HiDpi::*,
        Shell::{IVirtualDesktopManager, VirtualDesktopManager},
        WindowsAndMessaging::*,
    },
};
use rustty::config::{
    Config, QuickTerminalPosition as Position, QuickTerminalScreen as Selection,
    QuickTerminalSpaceBehavior,
};
use std::time::{Duration, Instant};

struct Screen {
    monitor: RECT,
    work: RECT,
    scale: f64,
}
fn screen(selection: Selection) -> Option<Screen> {
    unsafe {
        let handle = match selection {
            Selection::Mouse => {
                let mut point = POINT::default();
                GetCursorPos(&mut point).ok()?;
                MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY)
            }
            Selection::Main => MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY),
            Selection::MacosMenuBar => MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY),
        };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as _,
            ..Default::default()
        };
        GetMonitorInfoW(handle, &mut info).ok().ok()?;
        let mut x = 96;
        let mut y = 96;
        let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        Some(Screen {
            monitor: info.rcMonitor,
            work: info.rcWork,
            scale: f64::from(x.max(1)) / 96.0,
        })
    }
}
pub(super) fn frame(config: &Config, saved: Option<[f64; 2]>) -> Option<[f64; 4]> {
    let screen = screen(config.quick_terminal_screen)?;
    let rect = rect_frame(screen.work).map(|value| value / screen.scale);
    Some(geometry(rect, config.quick_terminal_position, saved))
}
fn geometry(work: [f64; 4], position: Position, saved: Option<[f64; 2]>) -> [f64; 4] {
    let [x, y, sw, sh] = work;
    let [width, height] = saved.unwrap_or(match position {
        Position::Top | Position::Bottom => [sw, sh * 0.5],
        Position::Left | Position::Right => [sw * 0.5, sh],
        Position::Center => [sw * 0.8, sh * 0.7],
    });
    let width = width.clamp(1.0, sw.max(1.0));
    let height = height.clamp(1.0, sh.max(1.0));
    let x = x + match position {
        Position::Left => 0.0,
        Position::Right => sw - width,
        _ => (sw - width) * 0.5,
    };
    let y = y + match position {
        Position::Top => 0.0,
        Position::Bottom => sh - height,
        _ => (sh - height) * 0.5,
    };
    [x, y, width, height]
}
fn rect_frame(rect: RECT) -> [f64; 4] {
    [
        rect.left as f64,
        rect.top as f64,
        (rect.right - rect.left) as f64,
        (rect.bottom - rect.top) as f64,
    ]
}
fn native_frame(hwnd: HWND) -> Result<[f64; 4], String> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(err)?;
    Ok(rect_frame(rect))
}
pub(super) fn saved_frame(hwnd: HWND) -> Result<[f64; 4], String> {
    let scale = f64::from(unsafe { GetDpiForWindow(hwnd) }.max(96)) / 96.0;
    Ok(native_frame(hwnd)?.map(|value| value / scale))
}
fn hidden(mut frame: [f64; 4], monitor: RECT, position: Position) -> [f64; 4] {
    match position {
        Position::Top => frame[1] = monitor.top as f64 - frame[3],
        Position::Bottom => frame[1] = monitor.bottom as f64,
        Position::Left => frame[0] = monitor.left as f64 - frame[2],
        Position::Right => frame[0] = monitor.right as f64,
        Position::Center => (),
    }
    frame
}
struct Animation {
    from: [f64; 4],
    to: [f64; 4],
    alpha_from: f64,
    alpha_to: f64,
    start: Instant,
    duration: Duration,
}
pub(super) struct QuickWindow {
    hwnd: HWND,
    previous: Option<HWND>,
    final_frame: Option<[f64; 4]>,
    monitor: Option<RECT>,
    animation: Option<Animation>,
    visible: bool,
    alpha: f64,
    layered: bool,
    added_layered: bool,
}
impl QuickWindow {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            previous: None,
            final_frame: None,
            monitor: None,
            animation: None,
            visible: false,
            alpha: 1.0,
            layered: false,
            added_layered: false,
        }
    }
    pub fn saved_frame(&self) -> Result<[f64; 4], String> {
        if self.animation.is_some()
            && let Some(frame) = self.final_frame
        {
            let scale = f64::from(unsafe { GetDpiForWindow(self.hwnd) }.max(96)) / 96.0;
            return Ok(frame.map(|value| value / scale));
        }
        saved_frame(self.hwnd)
    }
    pub fn show(&mut self, config: &Config) -> Result<(), String> {
        let screen =
            screen(config.quick_terminal_screen).ok_or("no Windows monitor is available")?;
        self.monitor = Some(screen.monitor);
        let saved = self.saved_frame()?;
        let target = geometry(
            rect_frame(screen.work).map(|v| v / screen.scale),
            config.quick_terminal_position,
            Some([saved[2], saved[3]]),
        )
        .map(|v| v * screen.scale);
        let foreground = unsafe { GetForegroundWindow() };
        if !self.visible {
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(foreground, Some(&mut pid));
            }
            self.previous =
                (pid != std::process::id() && !foreground.0.is_null()).then_some(foreground);
        }
        if config.quick_terminal_space_behavior == QuickTerminalSpaceBehavior::Move {
            // The Windows equivalent moves on reveal. It does not pin to every desktop.
            if let Err(error) = move_to_foreground_desktop(self.hwnd, foreground) {
                eprintln!("moving quick terminal to foreground desktop: {error}");
            }
        }
        let fade = config.quick_terminal_position == Position::Center;
        let from = if unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            native_frame(self.hwnd)?
        } else {
            self.alpha = if fade && !config.quick_terminal_animation_duration.is_zero() {
                0.0
            } else {
                1.0
            };
            hidden(target, screen.monitor, config.quick_terminal_position)
        };
        self.visible = true;
        self.final_frame = Some(target);
        self.start(
            from,
            target,
            1.0,
            config.quick_terminal_animation_duration,
            fade,
        )?;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        Ok(())
    }
    pub fn hide(&mut self, restore_focus: bool, config: &Config) -> Result<(), String> {
        let frame = if self.animation.is_some() {
            self.final_frame.unwrap_or(native_frame(self.hwnd)?)
        } else {
            native_frame(self.hwnd)?
        };
        if restore_focus
            && unsafe { GetForegroundWindow() } == self.hwnd
            && let Some(previous) = self
                .previous
                .take()
                .filter(|hwnd| unsafe { IsWindow(Some(*hwnd)) }.as_bool())
        {
            unsafe {
                let _ = SetForegroundWindow(previous);
            }
        }
        self.previous = None;
        self.visible = false;
        self.final_frame = Some(frame);
        let monitor = if self.animation.is_some() {
            self.monitor
        } else {
            None
        }
        .or_else(|| unsafe {
            let handle = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as _,
                ..Default::default()
            };
            GetMonitorInfoW(handle, &mut info)
                .as_bool()
                .then_some(info.rcMonitor)
        })
        .ok_or("no Windows monitor is available")?;
        let target = hidden(frame, monitor, config.quick_terminal_position);
        let fade = config.quick_terminal_position == Position::Center;
        self.start(
            native_frame(self.hwnd)?,
            target,
            if fade { 0.0 } else { 1.0 },
            config.quick_terminal_animation_duration,
            fade,
        )
    }
    fn start(
        &mut self,
        from: [f64; 4],
        to: [f64; 4],
        alpha_to: f64,
        duration: Duration,
        fade: bool,
    ) -> Result<(), String> {
        self.animation = None; // Reversal cancels the old completion unconditionally.
        if fade && !duration.is_zero() && !self.layered {
            unsafe {
                let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
                self.added_layered = style & WS_EX_LAYERED.0 as isize == 0;
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as isize);
            }
            self.layered = true;
        }
        if duration.is_zero() {
            self.finish()?;
        } else {
            self.apply(from, self.alpha)?;
            self.animation = Some(Animation {
                from,
                to,
                alpha_from: self.alpha,
                alpha_to,
                start: Instant::now(),
                duration,
            });
        }
        Ok(())
    }
    fn apply(&mut self, frame: [f64; 4], alpha: f64) -> Result<(), String> {
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                frame[0].round() as _,
                frame[1].round() as _,
                frame[2].round() as _,
                frame[3].round() as _,
                SWP_NOACTIVATE,
            )
            .map_err(err)?;
            if self.layered {
                SetLayeredWindowAttributes(
                    self.hwnd,
                    COLORREF(0),
                    (alpha.clamp(0.0, 1.0) * 255.0).round() as _,
                    LWA_ALPHA,
                )
                .map_err(err)?;
            }
        }
        self.alpha = alpha;
        Ok(())
    }
    fn finish(&mut self) -> Result<(), String> {
        self.animation = None;
        if !self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        if let Some(frame) = self.final_frame {
            self.apply(frame, 1.0)?;
        }
        if self.layered {
            if self.added_layered {
                unsafe {
                    let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style & !(WS_EX_LAYERED.0 as isize));
                }
            }
            self.layered = false;
            self.added_layered = false;
        }
        Ok(())
    }
    pub fn resigned_focus(&mut self) {
        self.previous = None;
    }
    pub fn tick(&mut self) {
        let Some(animation) = &self.animation else {
            return;
        };
        let fraction = animation.start.elapsed().as_secs_f64() / animation.duration.as_secs_f64();
        let result = if fraction >= 1.0 {
            self.finish()
        } else {
            let eased = 1.0 - (1.0 - fraction).powi(3);
            let frame = std::array::from_fn(|i| {
                animation.from[i] + (animation.to[i] - animation.from[i]) * eased
            });
            let alpha = animation.alpha_from + (animation.alpha_to - animation.alpha_from) * eased;
            self.apply(frame, alpha)
        };
        if let Err(error) = result {
            self.animation = None;
            eprintln!("quick terminal animation: {error}");
        }
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.animation
            .as_ref()
            .map(|a| (Instant::now() + Duration::from_millis(16)).min(a.start + a.duration))
    }
}
fn move_to_foreground_desktop(hwnd: HWND, foreground: HWND) -> ::windows::core::Result<()> {
    if foreground.0.is_null() {
        return Ok(());
    }
    unsafe {
        let manager: IVirtualDesktopManager =
            CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_INPROC_SERVER)?;
        let desktop = manager.GetWindowDesktopId(foreground)?;
        manager.MoveWindowToDesktop(hwnd, &desktop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geometry_anchors_and_clamps_on_negative_monitors() {
        assert_eq!(
            geometry(
                [-1920.0, -200.0, 1920.0, 1000.0],
                Position::Right,
                Some([640.0, 400.0])
            ),
            [-640.0, 100.0, 640.0, 400.0]
        );
        assert_eq!(
            geometry(
                [0.0, 40.0, 1280.0, 680.0],
                Position::Top,
                Some([2000.0, 1000.0])
            ),
            [0.0, 40.0, 1280.0, 680.0]
        );
    }
}
