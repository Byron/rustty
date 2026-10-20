//! POSIX regular expressions use the same platform contract as C tmux.
use std::ffi::CString;
use std::mem::MaybeUninit;

struct Regex(libc::regex_t);
impl Regex {
    fn new(pattern: &[u8], ignore_case: bool) -> Option<Self> {
        let pattern = CString::new(pattern).ok()?;
        let mut value = MaybeUninit::uninit();
        let flags = libc::REG_EXTENDED | if ignore_case { libc::REG_ICASE } else { 0 };
        if unsafe { libc::regcomp(value.as_mut_ptr(), pattern.as_ptr(), flags) } != 0 {
            return None;
        }
        Some(Self(unsafe { value.assume_init() }))
    }
    fn captures(&self, text: &[u8]) -> Option<[libc::regmatch_t; 10]> {
        let text = CString::new(text).ok()?;
        let mut captures = [libc::regmatch_t {
            rm_so: -1,
            rm_eo: -1,
        }; 10];
        (unsafe {
            libc::regexec(
                &self.0,
                text.as_ptr(),
                captures.len(),
                captures.as_mut_ptr(),
                0,
            )
        } == 0)
            .then_some(captures)
    }
}
impl Drop for Regex {
    fn drop(&mut self) {
        unsafe { libc::regfree(&mut self.0) };
    }
}

pub(super) fn matches(pattern: &[u8], text: &[u8], ignore_case: bool) -> bool {
    super::locale::with(|| {
        Regex::new(pattern, ignore_case).is_some_and(|regex| regex.captures(text).is_some())
    })
    .unwrap_or(false)
}

pub(super) fn substitute(
    pattern: &[u8],
    replacement: &[u8],
    text: &[u8],
    ignore_case: bool,
) -> Option<Vec<u8>> {
    super::locale::with(|| substitute_inner(pattern, replacement, text, ignore_case)).flatten()
}

fn substitute_inner(
    pattern: &[u8],
    replacement: &[u8],
    text: &[u8],
    ignore_case: bool,
) -> Option<Vec<u8>> {
    if pattern.is_empty() || text.is_empty() {
        return Some(text.to_vec());
    }
    let Some(regex) = Regex::new(pattern, ignore_case) else {
        return Some(text.to_vec());
    };
    let mut out = Vec::new();
    let (mut start, mut last, mut empty) = (0, 0, false);
    while start <= text.len() {
        let Some(captures) = regex.captures(&text[start..]) else {
            out.extend_from_slice(&text[start..]);
            break;
        };
        let whole = captures[0];
        let begin = start + whole.rm_so as usize;
        let end = start + whole.rm_eo as usize;
        out.extend_from_slice(&text[last..begin]);
        let anchored = pattern[0] == b'^';
        if anchored || empty || begin != last || begin != end {
            let mut cursor = 0;
            while cursor < replacement.len() {
                let mut byte = replacement[cursor];
                cursor += 1;
                if byte == b'\\' && cursor < replacement.len() {
                    byte = replacement[cursor];
                    cursor += 1;
                    if byte.is_ascii_digit() {
                        let group = captures[(byte - b'0') as usize];
                        if group.rm_so >= 0 && group.rm_so != group.rm_eo {
                            out.extend_from_slice(
                                &text[start + group.rm_so as usize..start + group.rm_eo as usize],
                            );
                            continue;
                        }
                    }
                }
                out.push(byte);
            }
            if anchored {
                out.extend_from_slice(&text[end..]);
                break;
            }
            last = end;
            start = end;
            empty = false;
        } else {
            last = end;
            start = end + 1;
            empty = true;
        }
        if out.len() > 1024 * 1024 {
            return None;
        }
    }
    (out.len() <= 1024 * 1024).then_some(out)
}
