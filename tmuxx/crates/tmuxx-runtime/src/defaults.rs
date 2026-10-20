#[cfg(unix)]
use std::ffi::CStr;
use tmuxx_core::Engine;

pub(super) fn apply(engine: &mut Engine) {
    engine
        .identity
        .insert("pid".into(), std::process::id().to_string());
    #[cfg(unix)]
    engine
        .identity
        .insert("uid".into(), unsafe { libc::getuid() }.to_string());
    #[cfg(windows)]
    if let Ok(sid) = super::windows::account_sid() {
        engine.identity.insert("uid".into(), sid);
    }
    if let Some(user) = user_name() {
        engine.identity.insert("user".into(), user);
    }
    if let Some(host) = host_name() {
        engine.identity.insert("host".into(), host);
    }
    #[cfg(windows)]
    if !engine.environment.contains_key("HOME")
        && let Some(home) = user_home(b"").and_then(|value| String::from_utf8(value).ok())
    {
        engine.environment.insert("HOME".into(), home);
    }

    let shell = engine
        .environment
        .get("SHELL")
        .filter(|shell| super::scheduler::suitable_shell(shell.as_bytes()))
        .cloned()
        .or_else(login_shell)
        .unwrap_or_else(system_shell);
    engine.options.insert("default-shell".into(), shell);
    if let Some(editor) = engine
        .environment
        .get("VISUAL")
        .or_else(|| engine.environment.get("EDITOR"))
    {
        engine
            .server_options
            .insert("editor".into(), editor.clone());
        let keys = if editor
            .rsplit(if cfg!(windows) {
                &['/', '\\'][..]
            } else {
                &['/'][..]
            })
            .next()
            .unwrap_or_default()
            .contains("vi")
        {
            "vi"
        } else {
            "emacs"
        };
        engine.options.insert("status-keys".into(), keys.into());
        engine
            .global_window_options
            .insert("mode-keys".into(), keys.into());
    }
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut host = vec![0u8; 256];
    if unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len()) } != 0 {
        return None;
    }
    host.truncate(host.iter().position(|b| *b == 0).unwrap_or(host.len()));
    String::from_utf8(host).ok()
}

#[cfg(unix)]
pub(super) fn system_shell() -> String {
    "/bin/sh".into()
}

#[cfg(windows)]
pub(super) fn system_shell() -> String {
    // Resolve the OS-provided system directory; do not search an SSH client's
    // PATH or trust a relative COMSPEC to choose an executable.
    let mut path = vec![0u16; 32768];
    let count = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            path.as_mut_ptr(),
            path.len() as u32,
        )
    };
    if count > 0
        && (count as usize) < path.len()
        && let Ok(path) = String::from_utf16(&path[..count as usize])
    {
        return format!("{path}\\cmd.exe");
    }
    // A Unicode system directory is part of Windows' supported installation
    // contract; startup validation rejects this unresolved fallback if not.
    "cmd.exe".into()
}

#[cfg(windows)]
pub(super) fn native_search_path() -> std::io::Result<String> {
    use windows_sys::Win32::System::SystemInformation::{
        GetSystemDirectoryW, GetWindowsDirectoryW,
    };
    fn directory(
        function: unsafe extern "system" fn(*mut u16, u32) -> u32,
    ) -> std::io::Result<String> {
        let mut buffer = vec![0u16; 32768];
        let length = unsafe { function(buffer.as_mut_ptr(), buffer.len() as u32) };
        if length == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if length as usize >= buffer.len() {
            return Err(std::io::Error::other(
                "native system directory exceeds Windows path bound",
            ));
        }
        let path = String::from_utf16(&buffer[..length as usize]).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "native system directory is not Unicode",
            )
        })?;
        if !std::path::Path::new(&path).is_absolute() || path.contains(';') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "native system directory cannot be represented as an absolute PATH entry",
            ));
        }
        Ok(path)
    }
    // Resolve trusted OS locations without a caller-controlled PATH, environment
    // expansion, hard-coded drive letter, or a Unix colon-separated fallback.
    Ok(format!(
        "{};{}",
        directory(GetSystemDirectoryW)?,
        directory(GetWindowsDirectoryW)?
    ))
}

#[cfg(windows)]
fn wide_name(function: unsafe extern "system" fn(*mut u16, *mut u32) -> i32) -> Option<String> {
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    if unsafe { function(buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    buffer.truncate(length as usize);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    String::from_utf16(&buffer).ok()
}

#[cfg(windows)]
fn host_name() -> Option<String> {
    wide_name(windows_sys::Win32::System::WindowsProgramming::GetComputerNameW)
}

#[cfg(windows)]
pub(super) fn user_name() -> Option<String> {
    wide_name(windows_sys::Win32::System::WindowsProgramming::GetUserNameW)
}

#[cfg(windows)]
fn login_shell() -> Option<String> {
    let shell = std::env::var("COMSPEC").ok()?;
    super::scheduler::suitable_shell(shell.as_bytes()).then_some(shell)
}

#[cfg(windows)]
pub(super) fn user_home(name: &[u8]) -> Option<Vec<u8>> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::UI::Shell::GetUserProfileDirectoryW;
    let name = std::str::from_utf8(name).ok()?;
    if !name.is_empty() {
        let sid = named_account_sid(name)?;
        if sid != super::windows::account_sid().ok()? {
            use windows_sys::Win32::System::Registry::{
                HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
            };
            let key: Vec<u16> =
                format!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList\\{sid}")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
            let name: Vec<u16> = "ProfileImagePath\0".encode_utf16().collect();
            let mut value = vec![0u16; 32768];
            let mut bytes = (value.len() * 2) as u32;
            if unsafe {
                RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    key.as_ptr(),
                    name.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                    std::ptr::null_mut(),
                    value.as_mut_ptr().cast(),
                    &mut bytes,
                )
            } != 0
            {
                return None;
            }
            value.truncate(bytes as usize / 2);
            while value.last() == Some(&0) {
                value.pop();
            }
            return Some(String::from_utf16(&value).ok()?.into_bytes());
        }
    }
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let _token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    if unsafe { GetUserProfileDirectoryW(token, path.as_mut_ptr(), &mut length) } == 0 {
        // Restricted automation/service tokens may intentionally have no loaded
        // profile. Preserve an explicit inherited profile directory in that
        // case; never fabricate C:\\Users\\<account>.
        return std::env::var("USERPROFILE")
            .ok()
            .filter(|path| std::path::Path::new(path).is_absolute())
            .map(String::into_bytes);
    }
    path.truncate(length as usize);
    while path.last() == Some(&0) {
        path.pop();
    }
    Some(String::from_utf16(&path).ok()?.into_bytes())
}

#[cfg(windows)]
fn named_account_sid(name: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::{Authorization::ConvertSidToStringSidW, LookupAccountNameW};
    if name.contains('\0') {
        return None;
    }
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut sid_bytes = 0;
    let mut domain_chars = 0;
    let mut usage = 0;
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_bytes,
            std::ptr::null_mut(),
            &mut domain_chars,
            &mut usage,
        );
    }
    if sid_bytes == 0 || sid_bytes > 65536 || domain_chars > 32768 {
        return None;
    }
    let mut sid = vec![0u8; sid_bytes as usize];
    let mut domain = vec![0u16; domain_chars as usize];
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_bytes,
            domain.as_mut_ptr(),
            &mut domain_chars,
            &mut usage,
        )
    } == 0
    {
        return None;
    }
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid.as_ptr().cast_mut().cast(), &mut text) } == 0 {
        return None;
    }
    let mut length = 0;
    while length < 256 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let result = if length < 256 {
        String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) }).ok()
    } else {
        None
    };
    unsafe {
        LocalFree(text.cast());
    }
    result
}

#[cfg(unix)]
pub(super) fn user_name() -> Option<String> {
    String::from_utf8(account_field(b"", |entry| entry.pw_name)?).ok()
}

#[cfg(unix)]
fn login_shell() -> Option<String> {
    let shell = account_field(b"", |entry| entry.pw_shell)?;
    super::scheduler::suitable_shell(&shell)
        .then(|| String::from_utf8(shell).ok())
        .flatten()
}

#[cfg(unix)]
pub(super) fn user_home(name: &[u8]) -> Option<Vec<u8>> {
    account_field(name, |entry| entry.pw_dir)
}

#[cfg(unix)]
fn account_field(name: &[u8], field: fn(&libc::passwd) -> *const libc::c_char) -> Option<Vec<u8>> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut size = 1024;
    loop {
        let mut buffer = vec![0u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        // The reentrant lookup keeps concurrent embedded hosts independent.
        let status = unsafe {
            if name.as_bytes().is_empty() {
                libc::getpwuid_r(
                    libc::getuid(),
                    entry.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut found,
                )
            } else {
                libc::getpwnam_r(
                    name.as_ptr(),
                    entry.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut found,
                )
            }
        };
        if status == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        if status != 0 || found.is_null() {
            return None;
        }
        let entry = unsafe { entry.assume_init() };
        let value = field(&entry);
        if value.is_null() {
            return None;
        }
        return Some(unsafe { CStr::from_ptr(value) }.to_bytes().to_vec());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn editor_precedence_uses_basename_and_preserves_empty_visual() {
        for (visual, editor, expected, keys) in [
            (None, None, "/usr/bin/vi", "emacs"),
            (None, Some("/usr/bin/vim -f"), "/usr/bin/vim -f", "vi"),
            (Some(""), Some("vim"), "", "emacs"),
            (
                Some("/vi/path/emacs"),
                Some("vi"),
                "/vi/path/emacs",
                "emacs",
            ),
            (Some("/bin/VI"), None, "/bin/VI", "emacs"),
        ] {
            let mut engine = Engine::default();
            engine.environment.insert("SHELL".into(), "/bin/sh".into());
            if let Some(value) = visual {
                engine.environment.insert("VISUAL".into(), value.into());
            }
            if let Some(value) = editor {
                engine.environment.insert("EDITOR".into(), value.into());
            }
            apply(&mut engine);
            assert_eq!(engine.server_options["editor"], expected);
            assert_eq!(engine.options["status-keys"], keys);
            assert_eq!(engine.global_window_options["mode-keys"], keys);
            assert_eq!(engine.options["default-shell"], "/bin/sh");
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    #[test]
    fn account_profile_shell_and_editor_defaults_use_native_identity() {
        assert!(super::super::scheduler::suitable_shell(
            system_shell().as_bytes()
        ));
        let username = user_name().expect("current Windows account name");
        let home = user_home(b"").expect("current account profile");
        assert_eq!(user_home(username.as_bytes()).as_ref(), Some(&home));
        assert!(std::path::Path::new(std::str::from_utf8(&home).unwrap()).is_absolute());
        let mut engine = Engine::default();
        engine
            .environment
            .insert("VISUAL".into(), r"C:\Tools\vim.exe".into());
        apply(&mut engine);
        assert!(engine.identity["uid"].starts_with("S-1-"));
        assert_eq!(engine.identity["user"], username);
        assert_eq!(engine.options["status-keys"], "vi");
        assert_eq!(engine.server_options["editor"], r"C:\Tools\vim.exe");
        assert_eq!(engine.environment["HOME"].as_bytes(), home);
        assert!(user_home(b"not-a-real-account-tmuxx-9e47b5e6").is_none());
    }
}
