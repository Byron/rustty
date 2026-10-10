//! POSIX Kitty transport; macOS shared-memory descriptors require mmap, not read.
use super::{Command, MAX_DATA, check_dimensions};
use std::{
    ffi::CString,
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
};

pub(super) fn read(cmd: &Command, name: &[u8]) -> Result<Vec<u8>, &'static str> {
    const INVALID: &str = "EINVAL: invalid data";
    if name.len() < 2 || name.len() > 255 || name[0] != b'/' || name[1..].contains(&b'/') {
        return Err(INVALID);
    }
    let name = CString::new(name).map_err(|_| INVALID)?;
    // SAFETY: name is NUL terminated and File owns the successful descriptor.
    let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        return Err(INVALID);
    }
    let file = unsafe { File::from_raw_fd(fd) };
    // The open descriptor keeps the object alive; consume its name on error too.
    unsafe {
        libc::shm_unlink(name.as_ptr());
    }
    let length =
        usize::try_from(file.metadata().map_err(|_| INVALID)?.len()).map_err(|_| INVALID)?;
    if length == 0 {
        return Err(INVALID);
    }
    let format = cmd.values.get(&b'f').copied().unwrap_or(32);
    let expected = if format == 100 {
        None
    } else {
        check_dimensions(cmd.n(b's'), cmd.n(b'v'))?;
        let bpp = match format {
            24 => 3,
            32 => 4,
            _ => return Err("EINVAL: unsupported format"),
        };
        Some(cmd.n(b's') as usize * cmd.n(b'v') as usize * bpp)
    };
    let start = cmd.n(b'O') as usize;
    let available = length.checked_sub(start).ok_or(INVALID)?;
    let size = if cmd.n(b'S') != 0 {
        cmd.n(b'S') as usize
    } else if cmd.n(b'o') == 0 {
        expected.unwrap_or(available)
    } else {
        available
    };
    if size > MAX_DATA || size > available {
        return Err(INVALID);
    }
    // SAFETY: map the statted object read-only; the validated range lies within it.
    // Copy before unmapping so later writes cannot alter retained render snapshots.
    unsafe {
        let mapped = libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        );
        if mapped == libc::MAP_FAILED {
            return Err(INVALID);
        }
        let data = std::slice::from_raw_parts(mapped.cast::<u8>().add(start), size).to_vec();
        libc::munmap(mapped, length);
        Ok(data)
    }
}
