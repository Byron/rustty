//! Private session dump files on Windows.

use ::windows::{
    Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        Security::{
            ACCESS_ALLOWED_ACE, ACL, Authorization::*, Cryptography::*, DACL_SECURITY_INFORMATION,
            EqualSid, GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::*,
        System::{
            Com::CoTaskMemFree,
            SystemServices::ACCESS_ALLOWED_ACE_TYPE,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
        UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    },
    core::{BOOL, PCWSTR, PWSTR},
};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
};

fn win_error(error: ::windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code >> 16 == 0x8007 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(error)
    }
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn raw(handle: &impl AsRawHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

fn owned(handle: HANDLE) -> OwnedHandle {
    // Every caller passes a successfully created, uniquely owned Windows handle.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}

fn denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

pub(super) fn unique_id() -> String {
    let mut bytes = [0u8; 16];
    unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .expect("Windows system random generator failed");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn user_temporary_directory() -> io::Result<PathBuf> {
    // Do not depend on a launching shell's TEMP override: all instances of this
    // account must discover the same saved sessions.
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }
        .map_err(win_error)?;
    let directory = PathBuf::from(OsString::from_wide(unsafe { path.as_wide() })).join("Temp");
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

/// Token information is pointer-aligned and keeps the embedded SID alive.
struct UserToken(Vec<usize>);

impl UserToken {
    fn for_process(process: HANDLE) -> io::Result<Self> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(win_error)?;
        let token = owned(token);
        let mut size = 0;
        let _ = unsafe { GetTokenInformation(raw(&token), TokenUser, None, 0, &mut size) };
        if size == 0 || size > 1024 * 1024 {
            return Err(io::Error::other("Cannot read the Windows process user"));
        }
        let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        unsafe {
            GetTokenInformation(
                raw(&token),
                TokenUser,
                Some(data.as_mut_ptr().cast()),
                size,
                &mut size,
            )
        }
        .map_err(win_error)?;
        Ok(Self(data))
    }

    fn current() -> io::Result<Self> {
        Self::for_process(unsafe { GetCurrentProcess() })
    }

    fn sid(&self) -> PSID {
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn sid_string(&self) -> io::Result<String> {
        let mut value = PWSTR::null();
        unsafe { ConvertSidToStringSidW(self.sid(), &mut value) }.map_err(win_error)?;
        let string = unsafe { value.to_string() }.map_err(io::Error::other);
        unsafe { LocalFree(Some(HLOCAL(value.0.cast()))) };
        string
    }
}

struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let sddl = wide(sddl);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor,
                None,
            )
        }
        .map_err(win_error)?;
        Ok(Self(descriptor))
    }

    fn private(inherit: bool) -> io::Result<Self> {
        let sid = UserToken::current()?.sid_string()?;
        let flags = if inherit { "OICI" } else { "" };
        Self::from_sddl(&format!("O:{sid}D:P(A;{flags};GA;;;{sid})"))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }

    fn dacl(&self) -> io::Result<*mut ACL> {
        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut dacl = std::ptr::null_mut();
        unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            .map_err(win_error)?;
        if !present.as_bool() || dacl.is_null() {
            return Err(denied("Visual-state access control is missing"));
        }
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

fn check_security(handle: HANDLE, kind: SE_OBJECT_TYPE) -> io::Result<()> {
    let user = UserToken::current()?;
    let mut owner = PSID::default();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            handle,
            kind,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            Some(&mut descriptor),
        )
    }
    .ok()
    .map_err(win_error)?;
    let descriptor = Descriptor(descriptor);
    if owner.0.is_null() || unsafe { EqualSid(owner, user.sid()) }.is_err() || dacl.is_null() {
        return Err(denied("Visual state is not private to the current user"));
    }
    let mut control = 0;
    let mut revision = 0;
    unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) }
        .map_err(win_error)?;
    if control & SE_DACL_PROTECTED.0 == 0 {
        return Err(denied("Visual state inherits external access permissions"));
    }
    // Reject unfamiliar ACE forms as well as access for other accounts. Our
    // protected descriptors contain only ordinary allow ACEs for this user.
    for index in 0..unsafe { (*dacl).AceCount } {
        let mut ace = std::ptr::null_mut();
        unsafe { GetAce(dacl, u32::from(index), &mut ace) }.map_err(win_error)?;
        let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
        if unsafe { (*ace).Header.AceType } != ACCESS_ALLOWED_ACE_TYPE as u8
            || unsafe { (*ace).Header.AceSize } < size_of::<ACCESS_ALLOWED_ACE>() as u16
            || unsafe {
                EqualSid(
                    PSID(std::ptr::addr_of!((*ace).SidStart).cast_mut().cast()),
                    user.sid(),
                )
            }
            .is_err()
        {
            return Err(denied("Visual state grants access to another account"));
        }
    }
    Ok(())
}

fn open_metadata(path: &Path) -> io::Result<OwnedHandle> {
    let path = wide(path);
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            (READ_CONTROL | FILE_READ_ATTRIBUTES).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map(owned)
    .map_err(win_error)
}

fn file_information(handle: HANDLE) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
        return Err(denied("Visual state must be stored in a regular file"));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(win_error)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(denied("Visual state cannot use a reparse point"));
    }
    Ok(info)
}

pub(super) fn private_directory(path: &Path) -> io::Result<()> {
    let descriptor = Descriptor::private(true)?;
    let attributes = descriptor.attributes();
    let name = wide(path);
    match unsafe { CreateDirectoryW(PCWSTR(name.as_ptr()), Some(&attributes)) }.map_err(win_error) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    check_private_directory(path)
}

pub(super) fn check_private_directory(path: &Path) -> io::Result<()> {
    let handle = open_metadata(path)?;
    let info = file_information(raw(&handle))?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err(denied("Unsafe visual-state directory"));
    }
    check_security(raw(&handle), SE_FILE_OBJECT)
}

pub(super) fn protect_file(file: &File) -> io::Result<()> {
    let info = file_information(raw(file))?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 || info.nNumberOfLinks != 1 {
        return Err(denied("Unsafe visual-state staging file"));
    }
    // std::fs opens without WRITE_DAC. Reopen this same object, rather than a
    // pathname that could have changed, to install a protected owner-only ACL.
    let handle = owned(
        unsafe {
            ReOpenFile(
                raw(file),
                (READ_CONTROL | WRITE_DAC | WRITE_OWNER).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAG_OPEN_REPARSE_POINT,
            )
        }
        .map_err(win_error)?,
    );
    let descriptor = Descriptor::private(false)?;
    let user = UserToken::current()?;
    unsafe {
        SetSecurityInfo(
            raw(&handle),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            Some(user.sid()),
            None,
            Some(descriptor.dacl()?),
            None,
        )
    }
    .ok()
    .map_err(win_error)
}

pub(super) fn open_private_file(path: &Path, limit: u64) -> io::Result<File> {
    check_private_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent"))?,
    )?;
    let path = wide(path);
    let handle = owned(
        unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(win_error)?,
    );
    let info = file_information(raw(&handle))?;
    let bytes = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
        || info.nNumberOfLinks != 1
        || bytes > limit
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsafe or oversized visual-state file",
        ));
    }
    check_security(raw(&handle), SE_FILE_OBJECT)?;
    Ok(File::from(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("rustty-handoff-security-{}", unique_id()));
            private_directory(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn private_file(path: &Path) -> File {
        let file = File::create(path).unwrap();
        protect_file(&file).unwrap();
        file
    }

    #[test]
    fn private_files_reject_hard_links_and_keep_delete_sharing() {
        let directory = Directory::new();
        let path = directory.0.join("payload");
        let file = private_file(&path);
        file.set_len(8).unwrap();
        assert!(open_private_file(&path, 7).is_err());
        let reader = open_private_file(&path, 8).unwrap();
        let link = directory.0.join("linked");
        fs::hard_link(&path, &link).unwrap();
        assert!(open_private_file(&path, 8).is_err());
        assert!(open_private_file(&link, 8).is_err());
        assert!(protect_file(&file).is_err());
        fs::remove_file(link).unwrap();
        let moved = directory.0.join("cancelled");
        fs::rename(&path, &moved).unwrap();
        fs::remove_file(moved).unwrap();
        drop(reader);
        drop(file);
    }

    #[test]
    fn private_file_checks_reject_permissions_for_other_accounts() {
        let directory = Directory::new();
        let path = directory.0.join("payload");
        let file = private_file(&path);
        let handle = owned(
            unsafe {
                ReOpenFile(
                    raw(&file),
                    (READ_CONTROL | WRITE_DAC | WRITE_OWNER).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                )
            }
            .unwrap(),
        );
        let user = UserToken::current().unwrap().sid_string().unwrap();
        let descriptor =
            Descriptor::from_sddl(&format!("D:P(A;;FA;;;{user})(A;;FR;;;WD)")).unwrap();
        unsafe {
            SetSecurityInfo(
                raw(&handle),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(descriptor.dacl().unwrap()),
                None,
            )
        }
        .ok()
        .unwrap();
        assert_eq!(
            open_private_file(&path, 10).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        protect_file(&file).unwrap();
        assert!(open_private_file(&path, 10).is_ok());
    }

    #[test]
    fn private_directories_reject_inherited_acl_and_reparse_points() {
        let directory = Directory::new();
        let inherited = directory.0.join("inherited");
        fs::create_dir(&inherited).unwrap();
        assert!(private_directory(&inherited).is_err());
        let link = directory.0.join("link");
        match std::os::windows::fs::symlink_dir(&inherited, &link) {
            Ok(()) => {
                assert!(check_private_directory(&link).is_err());
                fs::remove_dir(link).unwrap();
            }
            // Windows only permits symlink creation with Developer Mode or the
            // symlink privilege; the ACL assertion above still always runs.
            Err(error) if error.raw_os_error() == Some(1314) => {}
            Err(error) => panic!("create test symlink: {error}"),
        }
    }
}
