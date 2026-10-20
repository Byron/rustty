//! Linux-only executable provenance for live conformance fixtures.
//! Pin both process and socket inode so neither cleanup nor a later pairing can
//! silently select a replacement at the same path or a reused numeric PID.
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct SocketOwner {
    socket: PathBuf,
    inode: File,
    process: OwnedFd,
    pid: libc::pid_t,
    start: u64,
    executable: (u64, u64),
}
impl SocketOwner {
    pub fn capture(
        socket: &Path,
        executable: &Path,
        expected_pid: Option<u32>,
    ) -> io::Result<Self> {
        let inode = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(socket)?;
        let stream = UnixStream::connect(socket)?;
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if length as usize != size_of_val(&credentials)
            || credentials.pid <= 0
            || credentials.uid != unsafe { libc::geteuid() }
            || expected_pid.is_some_and(|pid| pid != credentials.pid as u32)
        {
            return Err(io::Error::other(format!(
                "unexpected owner for {}: PID {}, uid {}, expected PID {expected_pid:?}",
                socket.display(),
                credentials.pid,
                credentials.uid
            )));
        }
        let start = start_time(credentials.pid)?;
        // SO_PEERPIDFD (Linux 6.5+) obtains the peer's exact kernel identity.
        // The aarch64/x86_64 ABIs use asm-generic option number 77.
        // Older kernels retain explicit SO_PEERCRED/start-time revalidation.
        let (fd, identity_method) = peer_pidfd(&stream, credentials.pid)?;
        let process = unsafe { OwnedFd::from_raw_fd(fd) };
        if start_time(credentials.pid)? != start {
            return Err(io::Error::other("peer PID was reused during registration"));
        }
        let mut checked: libc::ucred = unsafe { std::mem::zeroed() };
        let mut checked_length = size_of_val(&checked) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut checked as *mut libc::ucred).cast(),
                &mut checked_length,
            )
        } != 0
            || checked_length != length
            || checked.pid != credentials.pid
            || checked.uid != credentials.uid
        {
            return Err(io::Error::other("socket peer changed during registration"));
        }
        let path = executable.canonicalize()?;
        let expected = fs::metadata(&path)?;
        let actual_path = PathBuf::from(format!("/proc/{}/exe", credentials.pid));
        let actual = fs::metadata(&actual_path)?;
        if (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) {
            return Err(io::Error::other(format!(
                "wrong executable behind {}: {:?}, expected {}",
                socket.display(),
                fs::read_link(actual_path)?,
                path.display()
            )));
        }
        let owner = Self {
            socket: socket.to_owned(),
            inode,
            process,
            pid: credentials.pid,
            start,
            executable: (actual.dev(), actual.ino()),
        };
        owner.check()?;
        eprintln!(
            "Linux fixture owner: socket={} pid={} start={} executable={} device={} inode={} identity={identity_method}",
            socket.display(),
            owner.pid,
            owner.start,
            path.display(),
            actual.dev(),
            actual.ino()
        );
        Ok(owner)
    }
    pub fn check(&self) -> io::Result<()> {
        if self.exited()? {
            return Err(io::Error::other(format!(
                "original server {} exited",
                self.pid
            )));
        }
        if start_time(self.pid)? != self.start {
            return Err(io::Error::other("server process identity changed"));
        }
        let executable = fs::metadata(format!("/proc/{}/exe", self.pid))?;
        if (executable.dev(), executable.ino()) != self.executable {
            return Err(io::Error::other("server executable changed"));
        }
        if self.endpoint_replaced()? || !self.socket.exists() {
            return Err(io::Error::other(format!(
                "server endpoint {} was replaced",
                self.socket.display()
            )));
        }
        Ok(())
    }
    /// A missing endpoint is safe to leave absent. Any other inode belongs to
    /// a different owner and must survive fixture directory cleanup.
    pub fn endpoint_replaced(&self) -> io::Result<bool> {
        let original = self.inode.metadata()?;
        let current = match fs::symlink_metadata(&self.socket) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok((original.dev(), original.ino()) != (current.dev(), current.ino()))
    }
    pub fn exited(&self) -> io::Result<bool> {
        let mut poll = libc::pollfd {
            fd: self.process.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 0) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(result > 0)
    }
    pub fn terminate(&self) -> io::Result<()> {
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            if self.exited()? {
                return Ok(());
            }
            if unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.process.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } < 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(());
                }
                return Err(error);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if self.exited()? {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Err(io::Error::other(format!(
            "owned server {} did not exit",
            self.pid
        )))
    }
}
fn peer_pidfd(stream: &UnixStream, pid: libc::pid_t) -> io::Result<(i32, &'static str)> {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    {
        let mut fd = -1i32;
        let mut length = size_of_val(&fd) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                77,
                (&mut fd as *mut i32).cast(),
                &mut length,
            )
        } == 0
        {
            if length as usize == size_of_val(&fd) && fd >= 0 {
                return Ok((fd, "SO_PEERPIDFD"));
            }
            if fd >= 0 {
                unsafe {
                    libc::close(fd);
                }
            }
            return Err(io::Error::other("invalid SO_PEERPIDFD result"));
        }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOPROTOOPT | libc::EINVAL)) {
            return Err(error);
        }
    }
    let _ = stream;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok((fd, "SO_PEERCRED+start-time+pidfd_open"))
    }
}

fn start_time(pid: libc::pid_t) -> io::Result<u64> {
    let stat = fs::read(format!("/proc/{pid}/stat"))?;
    let end = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or_else(|| io::Error::other("invalid process stat"))?;
    let field = stat[end + 1..]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .nth(19)
        .ok_or_else(|| io::Error::other("missing process start time"))?;
    std::str::from_utf8(field)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("invalid process start time"))
}
