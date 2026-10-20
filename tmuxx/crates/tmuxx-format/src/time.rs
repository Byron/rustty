use std::ffi::{CStr, CString};
use std::mem::MaybeUninit;

fn local(timestamp: libc::time_t) -> Option<libc::tm> {
    let mut local = MaybeUninit::uninit();
    #[cfg(unix)]
    if unsafe { libc::localtime_r(&timestamp, local.as_mut_ptr()) }.is_null() {
        return None;
    }
    #[cfg(windows)]
    if unsafe { libc::localtime_s(local.as_mut_ptr(), &timestamp) } != 0 {
        // UCRT limits epoch conversion to year 3000 although libc::tm can
        // represent much larger years. Gregorian date/weekday structure
        // repeats every 400 years; use a supported equivalent calendar for
        // the local timezone conversion, then restore the checked full year.
        const CYCLE: i128 = 146097 * 86400;
        const YEAR_2000: i128 = 946684800;
        let cycles = (i128::from(timestamp) - YEAR_2000).div_euclid(CYCLE);
        let shifted = i128::from(timestamp) - cycles * CYCLE;
        let shifted = libc::time_t::try_from(shifted).ok()?;
        if unsafe { libc::localtime_s(local.as_mut_ptr(), &shifted) } != 0 {
            return None;
        }
        let mut date = unsafe { local.assume_init() };
        date.tm_year = i32::try_from(i128::from(date.tm_year) + cycles * 400).ok()?;
        return Some(date);
    }
    Some(unsafe { local.assume_init() })
}
fn strftime(format: &[u8], local: &libc::tm) -> Vec<u8> {
    let Ok(format) = CString::new(format) else {
        return Vec::new();
    };
    let mut result = vec![0u8; 8192];
    #[cfg(unix)]
    let len = super::locale::with(|| unsafe {
        libc::strftime(
            result.as_mut_ptr().cast(),
            result.len(),
            format.as_ptr(),
            local,
        )
    })
    .unwrap_or(0);
    #[cfg(windows)]
    let len = super::locale::strftime(&mut result, &format, local);
    result.truncate(len);
    result
}
pub(super) fn expand(format: &[u8], timestamp: libc::time_t) -> Vec<u8> {
    local(timestamp).map_or_else(Vec::new, |local| strftime(format, &local))
}
pub(super) fn now() -> libc::time_t {
    unsafe { libc::time(std::ptr::null_mut()) }
}

pub(super) fn format(value: &[u8], flags: &[u8], template: Option<&[u8]>) -> Vec<u8> {
    let Some(timestamp) = std::str::from_utf8(value)
        .ok()
        .and_then(|s| s.parse::<libc::time_t>().ok())
        .filter(|n| *n > 0)
    else {
        return Vec::new();
    };
    if flags.contains(&b'p') {
        let now = now().max(timestamp);
        let (Some(date), Some(today)) = (local(timestamp), local(now)) else {
            return Vec::new();
        };
        let age = now - timestamp;
        let format: &[u8] = if age < 86400 {
            b"%H:%M"
        } else if (date.tm_year == today.tm_year && date.tm_mon == today.tm_mon) || age < 28 * 86400
        {
            b"%a%d"
        } else if (date.tm_year == today.tm_year && date.tm_mon < today.tm_mon)
            || (date.tm_year == today.tm_year - 1 && date.tm_mon > today.tm_mon)
        {
            b"%d%b"
        } else {
            b"%h%y"
        };
        return strftime(format, &date);
    }
    if flags.contains(&b'r') {
        let age = now() - timestamp;
        if age < 0 {
            return Vec::new();
        }
        let units = [
            (age / 86400, "d"),
            ((age % 86400) / 3600, "h"),
            ((age % 3600) / 60, "m"),
            (age % 60, "s"),
        ];
        for (index, (value, suffix)) in units.iter().enumerate() {
            if *value != 0 {
                let mut text = format!("{value}{suffix}");
                if let Some((next, suffix)) = units.get(index + 1)
                    && *next != 0
                {
                    text.push_str(&format!("{next}{suffix}"));
                }
                return text.into_bytes();
            }
        }
        return b"0s".to_vec();
    }
    if flags.contains(&b'd') {
        return (now() - timestamp).to_string().into_bytes();
    }
    if flags.contains(&b'f')
        && let Some(template) = template
    {
        return expand(template, timestamp);
    }
    let mut result = [0 as libc::c_char; 32];
    #[cfg(unix)]
    if unsafe { libc::ctime_r(&timestamp, result.as_mut_ptr()) }.is_null() {
        return Vec::new();
    }
    #[cfg(windows)]
    {
        unsafe extern "C" {
            fn _ctime64_s(output: *mut libc::c_char, count: usize, timestamp: *const i64) -> i32;
        }
        if unsafe { _ctime64_s(result.as_mut_ptr(), result.len(), &timestamp) } != 0 {
            return Vec::new();
        }
    }
    let result = unsafe { CStr::from_ptr(result.as_ptr()) }.to_bytes();
    result.strip_suffix(b"\n").unwrap_or(result).to_vec()
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn native_timestamp_templates_and_invalid_input_execute() {
        let local = libc::tm {
            tm_sec: 5,
            tm_min: 4,
            tm_hour: 3,
            tm_mday: 2,
            tm_mon: 0,
            tm_year: 124,
            tm_wday: 2,
            tm_yday: 1,
            tm_isdst: 0,
        };
        assert_eq!(
            super::strftime(b"%Y-%m-%dT%H:%M:%S", &local),
            b"2024-01-02T03:04:05"
        );
        assert_eq!(
            super::strftime("界/%Y/😀".as_bytes(), &local),
            "界/2024/😀".as_bytes()
        );
        assert_eq!(
            super::strftime(b"%h|%e|%D|%F|%R|%T|%u|%V", &local),
            b"Jan| 2|01/02/24|2024-01-02|03:04|03:04:05|2|01"
        );
        assert_eq!(super::format(b"1700000000", b"f", Some(b"%Y")), b"2023");
        assert_eq!(super::format(b"0", b"", None), b"");
        assert_eq!(super::format(b"-1", b"", None), b"");
        assert_eq!(super::format(b"invalid", b"", None), b"");
        assert_eq!(
            super::format(b"9223372036854775807", b"f", Some(b"%Y")),
            b""
        );
        assert_eq!(super::format(b"9223372036854775807", b"", None), b"");
        assert_eq!(super::format(b"3093527980800", b"f", Some(b"zz")), b"zz");
        assert_eq!(
            super::format(b"3093527980800", b"f", Some(b"%Y")),
            b"100000"
        );
        assert_eq!(
            super::format(b"9223372036854775807", b"f", Some(b"zz")),
            b""
        );
        for literal in [
            b"%".as_slice(),
            b"main:%0:run-shell",
            b"%1|%Q|%q|%N|%E|%O",
            "界%0/😀%q".as_bytes(),
        ] {
            assert_eq!(super::strftime(literal, &local), literal);
        }
        assert_eq!(
            super::strftime(b"%42|", &local),
            [vec![b' '; 38], b"%42|".to_vec()].concat()
        );
        assert_eq!(super::strftime(b"%999999999999999999999999q", &local), b"");
        assert_eq!(super::strftime(b"\0", &local), b"");
        assert!(!super::format(b"1700000000", b"", None).contains(&b'\n'));
        assert_eq!(
            crate::expand(b"#{t/f/%Y:stamp}", |_| Some(b"1700000000".to_vec()))
                .expect("valid timestamp"),
            b"2023"
        );
        assert_eq!(
            crate::expand(b"#{T:template}", |_| Some(b"%%/literal".to_vec()))
                .expect("valid time expansion"),
            b"%/literal"
        );
    }
}
