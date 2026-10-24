//! Private session dump files on macOS.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(super) fn unique_id() -> String {
    let mut bytes = [0u8; 16];
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn user_temporary_directory() -> io::Result<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let size = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
    if size == 0 || size > 1024 * 1024 {
        return Err(io::Error::other(
            "Cannot locate the macOS user temporary directory",
        ));
    }
    let mut bytes = vec![0; size];
    if unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            bytes.as_mut_ptr().cast(),
            size,
        )
    } != size
    {
        return Err(io::Error::other(
            "The macOS user temporary directory changed",
        ));
    }
    bytes.pop();
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

pub(super) fn private_directory(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    check_private_directory(path)
}

pub(super) fn check_private_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unsafe visual-state directory",
        ));
    }
    Ok(())
}

pub(super) fn open_private_file(path: &Path, limit: u64) -> io::Result<File> {
    check_private_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent"))?,
    )?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() > limit
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsafe or oversized visual-state file",
        ));
    }
    Ok(file)
}

pub(super) fn protect_file(file: &File) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
}
