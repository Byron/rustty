#[cfg(unix)]
use std::fs::Metadata;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub(super) fn diagnostic_path(path: &[u8]) -> String {
    std::str::from_utf8(path).map_or_else(|_| path.escape_ascii().to_string(), str::to_owned)
}

pub(super) fn error_message(error: &io::Error) -> String {
    #[cfg(windows)]
    if error.kind() == io::ErrorKind::NotFound {
        // Win32 distinguishes a missing file from a missing parent directory;
        // both represent the same command-level ENOENT contract in C tmux.
        return "No such file or directory".into();
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        return unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }
            .to_string_lossy()
            .into_owned();
    }
    error.to_string()
}

pub(super) fn path_error(error: &io::Error, path: &[u8]) -> String {
    format!("{}: {}", error_message(error), diagnostic_path(path))
}

#[cfg(unix)]
fn supported(metadata: &Metadata) -> bool {
    if metadata.is_file() {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if metadata.file_type().is_char_device() {
            return std::fs::metadata("/dev/null").is_ok_and(|null| null.rdev() == metadata.rdev());
        }
    }
    false
}

#[cfg(unix)]
fn validate(metadata: &Metadata) -> io::Result<()> {
    if supported(metadata) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported native file type: only regular files and /dev/null are supported",
        ))
    }
}

/// The owner may perform bounded regular I/O, but must never wait for a FIFO peer.
pub(super) fn open(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    return open_windows(options, path);
    #[cfg(unix)]
    {
        match std::fs::metadata(path) {
            Ok(metadata) => validate(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
        }
        let file = options.open(path)?;
        validate(&file.metadata()?)?;
        Ok(file)
    }
}

#[cfg(windows)]
fn open_windows(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_CHAR, FILE_TYPE_DISK, GetFileType};
    let null = validate_windows_path(path)?;
    let file = options.open(path)?;
    let kind = unsafe { GetFileType(file.as_raw_handle()) };
    if kind != FILE_TYPE_DISK && !(null && kind == FILE_TYPE_CHAR) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported native file type",
        ));
    }
    if kind == FILE_TYPE_DISK && !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native file operations require a regular file",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
pub(super) fn validate_windows_path(path: &Path) -> io::Result<bool> {
    // Inspect final-component links without opening their targets. In
    // particular a link to a named pipe must not consume a server connection
    // or wait for a peer before type admission rejects it.
    let mut resolved = path.to_path_buf();
    let mut null = false;
    for depth in 0..40 {
        null = windows_null(&resolved)?;
        if null {
            break;
        }
        match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if depth == 39 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "native file symbolic-link limit",
                    ));
                }
                let target = std::fs::read_link(&resolved)?;
                resolved = if target.is_absolute() {
                    target
                } else {
                    resolved.parent().unwrap_or(Path::new(".")).join(target)
                };
            }
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    Ok(null)
}

#[cfg(windows)]
fn windows_null(path: &Path) -> io::Result<bool> {
    let text = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows file paths must be Unicode",
        )
    })?;
    let lower = text.to_ascii_lowercase().replace('/', "\\");
    let device = lower
        .rsplit('\\')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("");
    let null = lower == "nul"
        || lower == r"\\.\nul"
        || (lower.rsplit('\\').next() == Some("nul")
            && matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))));
    if !null
        && (lower.starts_with(r"\\.\")
            || lower.starts_with(r"\\?\pipe\")
            || lower.starts_with(r"\device\")
            || matches!(device, "con" | "prn" | "aux" | "conin$" | "conout$" | "nul")
            || (device.len() == 4
                && (device.starts_with("com") || device.starts_with("lpt"))
                && device.as_bytes()[3].is_ascii_digit()))
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native file operations support regular files and NUL, not devices or named pipes",
        ));
    }
    Ok(null)
}
