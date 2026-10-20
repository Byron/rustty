//! Lexical Windows b/d format modifiers. No decoding, normalization or I/O.

struct Parts<'a> {
    path: &'a [u8],
    root: usize,
    end: usize,
    leaf: usize,
    verbatim: bool,
}

fn separator(byte: u8, verbatim: bool) -> bool {
    byte == b'\\' || (!verbatim && byte == b'/')
}

fn component_end(path: &[u8], start: usize, verbatim: bool) -> usize {
    path[start..]
        .iter()
        .position(|&byte| separator(byte, verbatim))
        .map_or(path.len(), |offset| start + offset)
}

fn drive(path: &[u8]) -> bool {
    path.first().is_some_and(u8::is_ascii_alphabetic) && path.get(1) == Some(&b':')
}

fn unc_end(path: &[u8], start: usize, verbatim: bool) -> Option<usize> {
    let server = component_end(path, start, verbatim);
    if server == start || server == path.len() {
        return None;
    }
    let share = component_end(path, server + 1, verbatim);
    (share > server + 1).then_some(share)
}

impl<'a> Parts<'a> {
    fn new(path: &'a [u8]) -> Self {
        let verbatim = path.starts_with(br"\\?\");
        let prefix = if verbatim {
            if path
                .get(4..8)
                .is_some_and(|part| part.eq_ignore_ascii_case(br"UNC\"))
            {
                unc_end(path, 8, true).unwrap_or_else(|| component_end(path, 4, true))
            } else if drive(&path[4..]) {
                6
            } else {
                component_end(path, 4, true)
            }
        } else if path.starts_with(br"\\.\") {
            component_end(path, 4, false)
        } else if path.len() >= 2 && separator(path[0], false) && separator(path[1], false) {
            unc_end(path, 2, false).unwrap_or(0)
        } else if drive(path) {
            2
        } else {
            0
        };
        let root = prefix
            + usize::from(
                path.get(prefix)
                    .is_some_and(|&byte| separator(byte, verbatim)),
            );
        let mut end = path.len();
        while end > root && separator(path[end - 1], verbatim) {
            end -= 1;
        }
        let leaf = path[root..end]
            .iter()
            .rposition(|&byte| separator(byte, verbatim))
            .map_or(root, |offset| root + offset + 1);
        Self {
            path,
            root,
            end,
            leaf,
            verbatim,
        }
    }

    fn root_or_dot(&self) -> Vec<u8> {
        if self.root == 0 {
            b".".to_vec()
        } else {
            self.path[..self.root].to_vec()
        }
    }
}

pub(super) fn basename(path: &[u8]) -> Vec<u8> {
    let parts = Parts::new(path);
    if parts.end == parts.root {
        parts.root_or_dot()
    } else {
        path[parts.leaf..parts.end].to_vec()
    }
}

pub(super) fn dirname(path: &[u8]) -> Vec<u8> {
    let parts = Parts::new(path);
    if parts.leaf == parts.root {
        return parts.root_or_dot();
    }
    let mut end = parts.leaf;
    while end > parts.root && separator(path[end - 1], parts.verbatim) {
        end -= 1;
    }
    path[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use crate::expand;

    fn check(path: &[u8], basename: &[u8], dirname: &[u8]) {
        assert_eq!(
            expand(b"#{b:path}", |_| Some(path.to_vec())).unwrap(),
            basename,
            "basename of {path:?}"
        );
        assert_eq!(
            expand(b"#{d:path}", |_| Some(path.to_vec())).unwrap(),
            dirname,
            "dirname of {path:?}"
        );
    }

    #[test]
    fn path_modifiers_cover_drive_relative_unc_and_extended_roots() {
        for (path, basename, dirname) in [
            ("", ".", "."),
            ("name", "name", "."),
            ("name\\", "name", "."),
            ("one\\\\two\\\\", "two", "one"),
            ("one/two", "two", "one"),
            ("\\", "\\", "\\"),
            ("///", "/", "/"),
            ("C:", "C:", "C:"),
            ("C:name", "name", "C:"),
            ("C:one\\two\\", "two", "C:one"),
            ("C:\\", "C:\\", "C:\\"),
            ("C:\\\\", "C:\\", "C:\\"),
            ("C:/", "C:/", "C:/"),
            ("C:/one\\two/", "two", "C:/one"),
            ("C:\\界\\😀\\", "😀", "C:\\界"),
            (
                "\\\\server\\share",
                "\\\\server\\share",
                "\\\\server\\share",
            ),
            (
                "\\\\server\\share\\\\",
                "\\\\server\\share\\",
                "\\\\server\\share\\",
            ),
            ("\\\\server\\share\\file", "file", "\\\\server\\share\\"),
            ("//server/share/file", "file", "//server/share/"),
            (
                "\\\\server\\share\\one\\two",
                "two",
                "\\\\server\\share\\one",
            ),
            ("\\\\?\\C:\\one\\two\\", "two", "\\\\?\\C:\\one"),
            ("\\\\?\\C:\\", "\\\\?\\C:\\", "\\\\?\\C:\\"),
            ("\\\\?\\C:\\one/two", "one/two", "\\\\?\\C:\\"),
            (
                "\\\\?\\UNC\\server\\share\\file",
                "file",
                "\\\\?\\UNC\\server\\share\\",
            ),
            (
                "\\\\?\\unc\\server\\share",
                "\\\\?\\unc\\server\\share",
                "\\\\?\\unc\\server\\share",
            ),
            ("\\\\?\\Volume{123}\\file", "file", "\\\\?\\Volume{123}\\"),
            ("\\\\.\\pipe\\name", "name", "\\\\.\\pipe\\"),
            ("\\\\.\\COM1", "\\\\.\\COM1", "\\\\.\\COM1"),
            ("one\\..", "..", "one"),
            ("one\\.\\file", "file", "one\\."),
            ("\\\\server", "server", "\\"),
            ("\\\\?\\UNC\\server", "server", "\\\\?\\UNC\\"),
        ] {
            check(path.as_bytes(), basename.as_bytes(), dirname.as_bytes());
        }
    }

    #[test]
    fn path_modifiers_preserve_malformed_bytes_and_literal_spelling() {
        check(b"C:\\parent\xff\\leaf\xfe", b"leaf\xfe", b"C:\\parent\xff");
        check(
            b"\\\\server\xff\\share\xfe\\leaf",
            b"leaf",
            b"\\\\server\xff\\share\xfe\\",
        );
        check(b"\\\\?\\C:\\raw\xff/name", b"raw\xff/name", b"\\\\?\\C:\\");
        check(b"relative\\nul\0byte", b"nul\0byte", b"relative");
        check("C:\\�\\é".as_bytes(), "é".as_bytes(), "C:\\�".as_bytes());
        check(b"C:\\a. \\leaf ", b"leaf ", b"C:\\a. ");
    }
}
