//! Native notifications and private files for macOS handoffs.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

const SIGNAL: libc::c_int = libc::SIGURG;

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

pub(super) fn process_identity(pid: u32) -> io::Result<(u64, u64)> {
    if pid <= 1 || pid > i32::MAX as u32 {
        return Err(io::Error::other("Invalid Rustty process identifier"));
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::pid_t,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        let error = io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(libc::ESRCH) {
            io::Error::new(io::ErrorKind::NotFound, error)
        } else {
            error
        });
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Rustty belongs to another user",
        ));
    }
    Ok((info.pbi_start_tvsec, info.pbi_start_tvusec))
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

pub(super) fn notify(pid: u32) -> io::Result<()> {
    // SIGURG is ignored by default: a PID-check/kill race must never terminate
    // an unrelated process that happens to reuse the PID.
    if unsafe { libc::kill(pid as libc::pid_t, SIGNAL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) struct Watcher {
    queue: Arc<OwnedFd>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    pub(super) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        static SIGNAL_INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
        let installed = SIGNAL_INSTALLED.get_or_init(|| {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = signal_handler as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(SIGNAL, &action, std::ptr::null_mut()) } != 0 {
                Err(io::Error::last_os_error().to_string())
            } else {
                Ok(())
            }
        });
        if let Err(error) = installed {
            return Err(io::Error::other(error.clone()));
        }
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let queue = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        let changes = [
            event(
                SIGNAL as usize,
                libc::EVFILT_SIGNAL,
                libc::EV_ADD | libc::EV_CLEAR,
                0,
            ),
            event(1, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0),
        ];
        if unsafe {
            libc::kevent(
                raw,
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_queue = Arc::clone(&queue);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("rustty-handoff-signal".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    let mut received = event(0, 0, 0, 0);
                    let count = unsafe {
                        libc::kevent(
                            thread_queue.as_raw_fd(),
                            std::ptr::null(),
                            0,
                            &mut received,
                            1,
                            std::ptr::null(),
                        )
                    };
                    if count < 0 {
                        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if !thread_stop.load(Ordering::Acquire)
                        && received.filter == libc::EVFILT_SIGNAL
                    {
                        wake();
                    }
                }
            })?;
        Ok(Self {
            queue,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let trigger = event(1, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
        unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &trigger,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            );
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

extern "C" fn signal_handler(_: libc::c_int) {}

fn event(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}
