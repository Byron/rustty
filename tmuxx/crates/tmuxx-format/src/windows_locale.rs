//! Explicit UCRT locales keep numeric and time formatting off the GUI's locale.
use std::ffi::{CString, c_char, c_void};

type Handle = *mut c_void;
type InvalidParameter = unsafe extern "C" fn(*const u16, *const u16, *const u16, u32, usize);

unsafe extern "C" {
    fn _create_locale(category: i32, locale: *const c_char) -> Handle;
    fn _free_locale(locale: Handle);
    fn _strtod_l(text: *const c_char, end: *mut *mut c_char, locale: Handle) -> f64;
    fn _strftime_l(
        output: *mut c_char,
        length: usize,
        format: *const c_char,
        local: *const libc::tm,
        locale: Handle,
    ) -> usize;
    fn _set_thread_local_invalid_parameter_handler(
        handler: Option<InvalidParameter>,
    ) -> Option<InvalidParameter>;
}

#[link(name = "legacy_stdio_definitions")]
unsafe extern "C" {
    fn _snprintf_l(
        output: *mut c_char,
        length: usize,
        format: *const c_char,
        locale: Handle,
        ...
    ) -> i32;
}

struct Locale(Handle);
impl Locale {
    fn numeric() -> Self {
        Self(unsafe { _create_locale(0, c"C".as_ptr()) })
    }

    fn time() -> Self {
        let name = ["LC_ALL", "LC_TIME", "LANG"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok())
            .find(|name| !name.is_empty())
            .unwrap_or_default();
        // The C locale's date words are ASCII in either codeset. UCRT does not
        // recognize the POSIX C.UTF-8 spelling accepted by Unix libc.
        let name = if matches!(name.as_str(), "C.UTF-8" | "C.utf8" | "C.UTF8" | "POSIX") {
            "C"
        } else {
            &name
        };
        let Ok(name) = CString::new(name) else {
            return Self(std::ptr::null_mut());
        };
        // LC_TIME = 5. Other categories retain C defaults in this private locale.
        Self(unsafe { _create_locale(5, name.as_ptr()) })
    }
}
impl Drop for Locale {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                _free_locale(self.0);
            }
        }
    }
}

thread_local! {
    static NUMERIC: Locale = Locale::numeric();
    static TIME: Locale = Locale::time();
}

pub(super) fn number(value: &[u8]) -> Option<f64> {
    let text = CString::new(value).ok()?;
    NUMERIC.with(|locale| {
        if locale.0.is_null() {
            return None;
        }
        let mut end = std::ptr::null_mut();
        let result = unsafe { _strtod_l(text.as_ptr(), &mut end, locale.0) };
        (unsafe { *end } == 0).then_some(result)
    })
}

pub(super) fn number_text(value: f64, precision: i32) -> Option<Vec<u8>> {
    // UCRT exposes NaN payload categories as "nan(ind)" or "nan(snan)".
    // tmux's numeric result is the signed canonical spelling used by its
    // supported Unix printf implementations, independent of the NaN payload.
    if value.is_nan() {
        return Some(if value.is_sign_negative() {
            b"-nan".to_vec()
        } else {
            b"nan".to_vec()
        });
    }
    NUMERIC.with(|locale| {
        if locale.0.is_null() {
            return None;
        }
        let mut output = vec![0u8; 512];
        let length = unsafe {
            _snprintf_l(
                output.as_mut_ptr().cast(),
                output.len(),
                c"%.*f".as_ptr(),
                locale.0,
                precision,
                value,
            )
        };
        if length < 0 || length as usize >= output.len() {
            return None;
        }
        output.truncate(length as usize);
        Some(output)
    })
}

unsafe extern "C" fn invalid_format(_: *const u16, _: *const u16, _: *const u16, _: u32, _: usize) {
}

struct RestoreHandler(Option<InvalidParameter>);
impl Drop for RestoreHandler {
    fn drop(&mut self) {
        unsafe {
            _set_thread_local_invalid_parameter_handler(self.0);
        }
    }
}

pub(super) fn strftime(output: &mut [u8], format: &std::ffi::CStr, local: &libc::tm) -> usize {
    // Unix strftime retains unknown conversions and incomplete '%' sequences.
    // UCRT instead rejects the entire string (and normally invokes the invalid
    // parameter handler). Protect only those literal sequences before calling
    // UCRT, so pane IDs such as %0 survive a second display/time expansion.
    let Some(format) = literal_time_directives(format.to_bytes(), output.len(), local) else {
        return 0;
    };
    TIME.with(|locale| {
        if locale.0.is_null() {
            return 0;
        }
        // Invalid calendar values and unsupported CRT combinations must never
        // invoke the embedding process's invalid-parameter handler.
        let _restore = RestoreHandler(unsafe {
            _set_thread_local_invalid_parameter_handler(Some(invalid_format))
        });
        unsafe {
            _strftime_l(
                output.as_mut_ptr().cast(),
                output.len(),
                format.as_ptr(),
                local,
                locale.0,
            )
        }
    })
}

fn literal_time_directives(format: &[u8], bound: usize, local: &libc::tm) -> Option<CString> {
    let mut prepared = Vec::with_capacity(format.len());
    let mut offset = 0;
    while offset < format.len() {
        if format[offset] != b'%' {
            prepared.push(format[offset]);
            offset += 1;
            continue;
        }
        let start = offset;
        offset += 1;
        let mut padding = b' ';
        let mut explicit_padding = false;
        let mut no_padding = false;
        while format
            .get(offset)
            .is_some_and(|value| b"_-0^#".contains(value))
        {
            if format[offset] == b'0' {
                padding = b'0';
                explicit_padding = true;
            } else if format[offset] == b'_' {
                padding = b' ';
                explicit_padding = true;
            } else if format[offset] == b'-' {
                no_padding = true;
            }
            offset += 1;
        }
        let mut width = 0usize;
        while format.get(offset).is_some_and(u8::is_ascii_digit) {
            width = width
                .checked_mul(10)?
                .checked_add(usize::from(format[offset] - b'0'))?;
            if width >= bound {
                return None;
            }
            offset += 1;
        }
        if format
            .get(offset)
            .is_some_and(|value| b"EO".contains(value))
        {
            offset += 1;
        }
        // UCRT rejects numeric full years above 9999. These POSIX numeric
        // fields have no locale-dependent words; preserve the actual checked
        // tm year instead of emitting the 400-year conversion surrogate.
        let year = i64::from(local.tm_year) + 1900;
        if !(0..=9999).contains(&year)
            && let Some(conversion @ (b'Y' | b'C' | b'y')) = format.get(offset).copied()
        {
            let (value, default_width) = match conversion {
                b'Y' => (year, 4),
                b'C' => (year.div_euclid(100), 2),
                _ => (year.rem_euclid(100), 2),
            };
            let digits = value.to_string();
            let width = if no_padding {
                width
            } else {
                width.max(default_width)
            };
            if !explicit_padding {
                padding = b'0';
            }
            if prepared.len().checked_add(width.max(digits.len()))? >= bound {
                return None;
            }
            prepared.extend(std::iter::repeat_n(
                padding,
                width.saturating_sub(digits.len()),
            ));
            prepared.extend_from_slice(digits.as_bytes());
            offset += 1;
            continue;
        }
        let known = format
            .get(offset)
            .is_some_and(|value| b"aAbBcCdDeFgGhHIjklmMnpPrRStTuUVwWxXyYzZs%".contains(value));
        if !known {
            let literal_length = (offset + usize::from(offset < format.len())) - start;
            prepared.extend(std::iter::repeat_n(
                padding,
                width.saturating_sub(literal_length),
            ));
            prepared.push(b'%');
        }
        // Include the conversion if present. The ordinary known case includes %%.
        if offset < format.len() {
            offset += 1;
        }
        prepared.extend_from_slice(&format[start..offset]);
    }
    Some(CString::new(prepared).expect("CStr input cannot contain interior NUL"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn c_numeric_grammar_and_rounding_preserve_locale_and_reject_trailing_data() {
        assert_eq!(super::number(b"0x1.8p+2"), Some(6.0));
        assert_eq!(super::number(b""), Some(0.0));
        assert_eq!(super::number(b" 2.5"), Some(2.5));
        assert_eq!(super::number(b"2.5 "), None);
        assert_eq!(super::number(b"2\0"), None);
        assert_eq!(
            super::number_text(1.0 / 3.0, 3).as_deref(),
            Some(b"0.333".as_slice())
        );
        assert_eq!(
            super::number_text(1.0, -1).as_deref(),
            Some(b"1.000000".as_slice())
        );
        assert_eq!(super::number_text(0.5, 0).as_deref(), Some(b"0".as_slice()));
        assert_eq!(super::number_text(2.5, 0).as_deref(), Some(b"2".as_slice()));
        for bits in [0x7ff8000000000000, 0x7ff0000000000001, 0x7ff8000000001234] {
            assert_eq!(
                super::number_text(f64::from_bits(bits), 3).as_deref(),
                Some(b"nan".as_slice())
            );
            assert_eq!(
                super::number_text(f64::from_bits(bits | (1 << 63)), 3).as_deref(),
                Some(b"-nan".as_slice())
            );
        }
    }

    #[test]
    fn native_formatting_restores_the_embedding_thread_handler_and_locale() {
        use std::ffi::CStr;
        let before =
            unsafe { CStr::from_ptr(libc::setlocale(libc::LC_ALL, std::ptr::null())) }.to_owned();
        let previous = unsafe {
            super::_set_thread_local_invalid_parameter_handler(Some(super::invalid_format))
        };
        let _restore = super::RestoreHandler(previous);
        let expected = Some(super::invalid_format as super::InvalidParameter);
        assert_eq!(
            crate::expand_time(b"%", |_| None).expect("invalid directive is bounded"),
            b"%"
        );
        let invalid = libc::tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 1,
            tm_mon: 0,
            tm_year: 124,
            tm_wday: 99,
            tm_yday: 0,
            tm_isdst: 0,
        };
        assert_eq!(super::strftime(&mut [0; 64], c"%A", &invalid), 0);
        let restored = unsafe { super::_set_thread_local_invalid_parameter_handler(expected) };
        assert_eq!(
            restored.map(|handler| handler as usize),
            expected.map(|handler| handler as usize)
        );
        assert_eq!(super::number(b"2.5"), Some(2.5));
        let after =
            unsafe { CStr::from_ptr(libc::setlocale(libc::LC_ALL, std::ptr::null())) }.to_owned();
        assert_eq!(before, after);
    }
}
