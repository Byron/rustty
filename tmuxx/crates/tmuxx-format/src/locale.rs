//! Scope POSIX text operations to tmux's locale without changing the GUI's locale.

struct Locale(libc::locale_t);
impl Locale {
    fn new() -> Self {
        for name in [c"en_US.UTF-8", c"C.UTF-8"] {
            let locale = unsafe {
                libc::newlocale(libc::LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut())
            };
            if !locale.is_null() {
                let with_time =
                    unsafe { libc::newlocale(libc::LC_TIME_MASK, c"".as_ptr(), locale) };
                return Self(if with_time.is_null() {
                    locale
                } else {
                    with_time
                });
            }
        }
        Self(std::ptr::null_mut())
    }
}
impl Drop for Locale {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                libc::freelocale(self.0);
            }
        }
    }
}
struct Restore(libc::locale_t);
impl Drop for Restore {
    fn drop(&mut self) {
        unsafe {
            libc::uselocale(self.0);
        }
    }
}
thread_local! { static LOCALE: Locale = Locale::new(); }

pub(super) fn with<T>(action: impl FnOnce() -> T) -> Option<T> {
    LOCALE.with(|locale| {
        if locale.0.is_null() {
            return None;
        }
        let previous = unsafe { libc::uselocale(locale.0) };
        if previous.is_null() {
            return None;
        }
        let _restore = Restore(previous);
        Some(action())
    })
}
