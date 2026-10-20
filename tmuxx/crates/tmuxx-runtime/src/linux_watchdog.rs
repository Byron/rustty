//! Linux PTY spawn broker and owner-lifetime watchdog.
//!
//! The broker is the actual parent of each PTY leader. `waitid(WNOWAIT)` reports
//! exits without releasing the leader's PID until its registration is removed.
//! This pins the session identity even after the leader exits. Cleanup uses
//! pidfds, never a potentially reused process-group number. The broker records
//! a child before it can execute, including when the owner's spawn reply is lost.
//!
//! After fork the helper uses only preallocated buffers, arithmetic and libc
//! syscall wrappers. No Rust allocator, inherited locks or destructors run there.
use portable_pty::{Child, ChildKiller, CommandBuilder, ExitStatus};
use std::{
    ffi::{CString, OsStr},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream, process::ExitStatusExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_SESSIONS: usize = 16_384;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_STRINGS: usize = 65_536;
const SPAWN: u64 = 1;
const STATUS: u64 = 2;
const RELEASE: u64 = 3;
const SIGNAL: u64 = 4;
const KILL_LEADER: u64 = 5;
// Header: operation, generation, pid, payload length, argc/signal, envc.
type Header = [u64; 6];
// Reply: errno, generation, pid, raw wait status (-1 while running).
type Reply = [i64; 4];

#[derive(Clone, Copy, Default)]
struct Session {
    pid: libc::pid_t,
    generation: u64,
}

#[derive(Debug)]
pub(super) struct Watchdog {
    socket: Mutex<Option<UnixStream>>,
    #[cfg(test)]
    pid: libc::pid_t,
    identity: OwnedFd,
}
#[derive(Debug)]
struct Lease {
    watchdog: Arc<Watchdog>,
    pid: libc::pid_t,
    generation: u64,
}
#[derive(Clone, Debug)]
pub(super) struct Registration(Arc<Lease>);
impl Registration {
    pub(super) fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        self.0.request(SIGNAL, signal as u64).map(|_| ())
    }
}
impl Lease {
    fn request(&self, operation: u64, argument: u64) -> io::Result<Reply> {
        self.watchdog.request(
            [operation, self.generation, self.pid as u64, 0, argument, 0],
            &[],
            None,
        )
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.request(RELEASE, 0);
    }
}

impl Watchdog {
    pub(super) fn start() -> io::Result<Arc<Self>> {
        // A procfs mounted for an ancestor PID namespace would enumerate
        // numbers that name different processes in this namespace.
        let stat = std::fs::read("/proc/self/stat")?;
        let mounted_pid = stat
            .split(|byte| *byte == b' ')
            .next()
            .and_then(|pid| std::str::from_utf8(pid).ok())
            .and_then(|pid| pid.parse::<libc::pid_t>().ok());
        if mounted_pid != Some(unsafe { libc::getpid() }) {
            return Err(io::Error::other(
                "Linux PTY broker requires /proc mounted for its PID namespace",
            ));
        }
        // Fail startup explicitly if the kernel or sandbox lacks pidfd support.
        let probe = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) } as i32;
        if probe < 0 {
            return Err(io::Error::last_os_error());
        }
        let owner_identity = unsafe { OwnedFd::from_raw_fd(probe) };
        let copied = unsafe { libc::fcntl(owner_identity.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if copied < 0 {
            return Err(io::Error::last_os_error());
        }
        let owner_identity = unsafe { OwnedFd::from_raw_fd(copied) };
        let signal_probe = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                owner_identity.as_raw_fd(),
                0,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        let probe_error = (signal_probe < 0).then(io::Error::last_os_error);
        if let Some(error) = probe_error {
            return Err(error);
        }
        // P_PIDFD arrived in Linux 5.4. Our own process is not our child, so
        // ECHILD proves the kernel understood this identity-safe wait API.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let wait_probe = unsafe {
            libc::waitid(
                libc::P_PIDFD,
                owner_identity.as_raw_fd() as u32,
                &mut info,
                libc::WEXITED | libc::WNOHANG,
            )
        };
        if wait_probe >= 0 || errno() != libc::ECHILD {
            return Err(io::Error::other(
                "Linux PTY broker requires waitid(P_PIDFD) support",
            ));
        }
        let (mut parent, initial_child) = UnixStream::pair()?;
        let copied = unsafe { libc::fcntl(initial_child.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if copied < 0 {
            return Err(io::Error::last_os_error());
        }
        let child = unsafe { UnixStream::from_raw_fd(copied) };
        drop(initial_child);
        parent.set_read_timeout(Some(Duration::from_secs(5)))?;
        parent.set_write_timeout(Some(Duration::from_secs(5)))?;
        child.set_read_timeout(Some(Duration::from_secs(5)))?;
        child.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut sessions = vec![Session::default(); MAX_SESSIONS];
        let mut payload = vec![0u8; MAX_BYTES];
        let mut pointers = vec![std::ptr::null::<libc::c_char>(); MAX_STRINGS + 4];
        let fd = child.as_raw_fd();
        let descriptors = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) }.max(0) as i32;
        let mut blocked = unsafe { std::mem::zeroed() };
        let mut original = unsafe { std::mem::zeroed() };
        let error = unsafe {
            libc::sigfillset(&mut blocked);
            libc::pthread_sigmask(libc::SIG_SETMASK, &blocked, &mut original)
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let mut broker_identity = -1;
        let pid = unsafe { fork_process(&mut broker_identity) };
        if pid == 0 {
            unsafe {
                if libc::setsid() < 0 {
                    libc::_exit(1);
                }
                // Ignored SIGCHLD/SA_NOCLDWAIT would destroy our PID reservation.
                if !reset_signal(libc::SIGCHLD) {
                    let mut error = errno();
                    transfer(fd, (&mut error as *mut i32).cast(), 4, true);
                    libc::_exit(1);
                }
                close_except(fd, owner_identity.as_raw_fd(), descriptors);
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if null < 0 {
                    libc::_exit(1);
                }
                for target in 0..3 {
                    if libc::dup2(null, target) < 0 {
                        libc::_exit(1);
                    }
                }
                if null > 2 {
                    libc::close(null);
                }
                child_loop(
                    fd,
                    owner_identity.as_raw_fd(),
                    &mut sessions,
                    &mut payload,
                    &mut pointers,
                    descriptors,
                );
            }
        }
        let error = (pid < 0).then(io::Error::last_os_error);
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &original, std::ptr::null_mut());
        }
        if let Some(error) = error {
            return Err(error);
        }
        let broker_identity = unsafe { OwnedFd::from_raw_fd(broker_identity) };
        drop(child);
        let mut ready = [0u8; 4];
        if let Err(error) = parent.read_exact(&mut ready) {
            signal_pidfd(broker_identity.as_raw_fd(), libc::SIGKILL);
            wait_broker(broker_identity.as_raw_fd(), Duration::from_secs(1));
            return Err(error);
        }
        let error = i32::from_ne_bytes(ready);
        if error != 0 {
            wait_broker(broker_identity.as_raw_fd(), Duration::from_secs(1));
            return Err(io::Error::from_raw_os_error(error));
        }
        Ok(Arc::new(Self {
            socket: Mutex::new(Some(parent)),
            #[cfg(test)]
            pid,
            identity: broker_identity,
        }))
    }

    pub(super) fn spawn(
        self: &Arc<Self>,
        command: CommandBuilder,
        master: i32,
    ) -> io::Result<(Box<dyn Child + Send + Sync>, Registration)> {
        let argv = command.get_argv();
        let cwd = command
            .get_cwd()
            .map(|p| p.as_os_str())
            .unwrap_or(OsStr::new("/"));
        let program = argv
            .first()
            .ok_or_else(|| io::Error::other("empty PTY command"))?;
        let executable = resolve(program, cwd, command.get_env("PATH"))?;
        let mut payload = Vec::new();
        let mut append = |bytes: &[u8]| -> io::Result<()> {
            if bytes.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "NUL in PTY spawn",
                ));
            }
            payload.extend_from_slice(bytes);
            payload.push(0);
            Ok(())
        };
        append(executable.as_os_str().as_bytes())?;
        append(cwd.as_bytes())?;
        for arg in argv {
            append(arg.as_bytes())?;
        }
        let mut envc = 0;
        for (key, value) in command.iter_full_env_as_str() {
            append(format!("{key}={value}").as_bytes())?;
            envc += 1;
        }
        if payload.len() > MAX_BYTES || argv.len() + envc + 2 > MAX_STRINGS {
            return Err(io::Error::from_raw_os_error(libc::E2BIG));
        }
        let reply = self.request(
            [
                SPAWN,
                0,
                0,
                payload.len() as u64,
                argv.len() as u64,
                envc as u64,
            ],
            &payload,
            Some(master),
        )?;
        let registration = Registration(Arc::new(Lease {
            watchdog: self.clone(),
            generation: reply[1] as u64,
            pid: reply[2] as i32,
        }));
        Ok((Box::new(BrokerChild(registration.clone())), registration))
    }

    fn request(
        &self,
        mut header: Header,
        payload: &[u8],
        descriptor: Option<i32>,
    ) -> io::Result<Reply> {
        let mut guard = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let socket = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("PTY broker closed"))?;
        let result = (|| {
            let fd = socket.as_raw_fd();
            write_request(fd, &mut header, payload, descriptor)?;
            let mut reply = [0i64; 4];
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(reply.as_mut_ptr().cast::<u8>(), size_of::<Reply>())
            };
            socket.read_exact(bytes)?;
            Ok(reply)
        })();
        match result {
            Ok(reply) if reply[0] != 0 => Err(io::Error::from_raw_os_error(reply[0] as i32)),
            Ok(reply) => Ok(reply),
            Err(error) => {
                // A partial request/reply cannot be retried. EOF makes the
                // broker clean even a child whose spawn acknowledgement was lost.
                if let Some(socket) = guard.take() {
                    let _ = socket.shutdown(std::net::Shutdown::Both);
                }
                Err(error)
            }
        }
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.socket
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if !wait_broker(self.identity.as_raw_fd(), Duration::from_secs(3)) {
            signal_pidfd(self.identity.as_raw_fd(), libc::SIGKILL);
            wait_broker(self.identity.as_raw_fd(), Duration::from_secs(1));
        }
    }
}
#[derive(Debug)]
struct BrokerChild(Registration);
impl ChildKiller for BrokerChild {
    fn kill(&mut self) -> io::Result<()> {
        self.0.0.request(KILL_LEADER, 0).map(|_| ())
    }
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self(self.0.clone()))
    }
}
impl Child for BrokerChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let reply = self.0.0.request(STATUS, 0)?;
        Ok((reply[3] >= 0).then(|| std::process::ExitStatus::from_raw(reply[3] as i32).into()))
    }
    fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn process_id(&self) -> Option<u32> {
        Some(self.0.0.pid as u32)
    }
}

fn write_request(
    fd: i32,
    header: &mut Header,
    payload: &[u8],
    descriptor: Option<i32>,
) -> io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: header.as_mut_ptr().cast(),
        iov_len: size_of::<Header>(),
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    if let Some(descriptor) = descriptor {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<i32>() as u32) } as usize;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&message);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as u32) as usize;
            libc::CMSG_DATA(cmsg).cast::<i32>().write(descriptor);
        }
    }
    let sent = loop {
        let n = unsafe { libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL) };
        if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if n <= 0 {
            return Err(io::Error::last_os_error());
        }
        break n as usize;
    };
    if !unsafe {
        transfer(
            fd,
            header.as_mut_ptr().cast::<u8>().add(sent),
            size_of::<Header>() - sent,
            true,
        )
    } || !unsafe { transfer(fd, payload.as_ptr().cast_mut(), payload.len(), true) }
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn resolve(program: &OsStr, cwd: &OsStr, path: Option<&OsStr>) -> io::Result<PathBuf> {
    let usable = |path: &Path| -> io::Result<bool> {
        let name = CString::new(path.as_os_str().as_bytes())?;
        Ok(!path.is_dir() && unsafe { libc::access(name.as_ptr(), libc::X_OK) } == 0)
    };
    let program_path = Path::new(program);
    if program.as_bytes().contains(&b'/') {
        let candidate = Path::new(cwd).join(program_path);
        if usable(&candidate)? {
            return Ok(candidate);
        }
    } else if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            let candidate = Path::new(cwd).join(dir).join(program);
            if usable(&candidate)? {
                return Ok(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "Unable to spawn {}: no executable found",
            program_path.display()
        ),
    ))
}

// fork-like clone with no shared VM. Unlike libc::fork this does not invoke
// application pthread_atfork callbacks in the multithreaded embedding host or
// inherit callbacks that could allocate or deadlock in the isolated helper.
unsafe fn fork_process(identity: *mut i32) -> libc::pid_t {
    let flags = libc::SIGCHLD
        | if identity.is_null() {
            0
        } else {
            libc::CLONE_PIDFD
        };
    unsafe {
        libc::syscall(libc::SYS_clone, flags, 0usize, identity, 0usize, 0usize) as libc::pid_t
    }
}

unsafe fn reset_signal(signal: i32) -> bool {
    // libc::signal/sigaction may acquire glibc's SIGABRT handler lock. An
    // unrelated host thread could have held that lock when clone copied it.
    // SIG_DFL, empty mask, flags and restorer are all zero on Linux; this
    // zero-filled buffer covers the kernel sigaction ABI on 32/64-bit targets.
    let action = [0u64; 4];
    unsafe {
        libc::syscall(
            libc::SYS_rt_sigaction,
            signal,
            action.as_ptr(),
            std::ptr::null::<u8>(),
            8usize,
        ) == 0
    }
}

fn signal_pidfd(fd: i32, signal: i32) {
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd,
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        );
    }
}
fn wait_broker(fd: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PIDFD,
                fd as u32,
                &mut info,
                libc::WEXITED | libc::WNOHANG,
            )
        };
        if result == 0 && unsafe { info.si_pid() } != 0 {
            return true;
        }
        if result < 0 {
            if errno() == libc::ECHILD {
                // A host SIGCHLD handler (or SA_NOCLDWAIT) can take reaping
                // responsibility while the helper is still alive. Its pidfd
                // remains the authority for actual exit, without PID reuse.
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut poll, 1, 0) } > 0 {
                    return true;
                }
            } else if errno() != libc::EINTR {
                return false;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}
unsafe fn transfer(fd: i32, mut bytes: *mut u8, mut len: usize, write: bool) -> bool {
    while len != 0 {
        let count = if write {
            unsafe { libc::send(fd, bytes.cast(), len, libc::MSG_NOSIGNAL) }
        } else {
            unsafe { libc::read(fd, bytes.cast(), len) }
        };
        if count < 0 && errno() == libc::EINTR {
            continue;
        }
        if count <= 0 {
            return false;
        }
        bytes = unsafe { bytes.add(count as usize) };
        len -= count as usize;
    }
    true
}
unsafe fn close_except(first: i32, second: i32, descriptors: i32) {
    let low = first.min(second).max(0);
    let high = first.max(second);
    // close_range is a direct syscall and also closes descriptors above a
    // subsequently lowered RLIMIT_NOFILE. Old kernels use the known limit.
    let mut success = true;
    for (from, to) in [(0, low - 1), (low + 1, high - 1), (high + 1, i32::MAX)] {
        if from <= to
            && unsafe { libc::syscall(libc::SYS_close_range, from as u32, to as u32, 0) } < 0
        {
            success = false;
        }
    }
    if second < 0 && first != 0 {
        unsafe {
            libc::close(0);
        }
    }
    if !success {
        for fd in 0..descriptors {
            if fd != first && fd != second {
                unsafe {
                    libc::close(fd);
                }
            }
        }
    }
}

unsafe fn receive_header(fd: i32, header: &mut Header) -> Option<i32> {
    let mut control = [0usize; 8];
    let mut iov = libc::iovec {
        iov_base: header.as_mut_ptr().cast(),
        iov_len: size_of::<Header>(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = size_of_val(&control);
    let count = loop {
        let n = unsafe { libc::recvmsg(fd, &mut message, libc::MSG_CMSG_CLOEXEC) };
        if n < 0 && errno() == libc::EINTR {
            continue;
        }
        if n <= 0 {
            return None;
        }
        break n as usize;
    };
    let mut descriptor = -1;
    let mut extra = false;
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !cmsg.is_null() {
        unsafe {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let count = ((*cmsg).cmsg_len - libc::CMSG_LEN(0) as usize) / size_of::<i32>();
                for index in 0..count {
                    let received = libc::CMSG_DATA(cmsg).cast::<i32>().add(index).read();
                    if descriptor < 0 {
                        descriptor = received;
                    } else {
                        libc::close(received);
                        extra = true;
                    }
                }
            }
            cmsg = libc::CMSG_NXTHDR(&message, cmsg);
        }
    }
    if extra
        || message.msg_flags & libc::MSG_CTRUNC != 0
        || !unsafe {
            transfer(
                fd,
                header.as_mut_ptr().cast::<u8>().add(count),
                size_of::<Header>() - count,
                false,
            )
        }
    {
        if descriptor >= 0 {
            unsafe {
                libc::close(descriptor);
            }
        }
        return None;
    }
    Some(descriptor)
}

unsafe fn child_loop(
    fd: i32,
    owner_identity: i32,
    sessions: &mut [Session],
    payload: &mut [u8],
    pointers: &mut [*const libc::c_char],
    descriptors: i32,
) -> ! {
    let proc = unsafe {
        libc::open(
            c"/proc".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    let mut ready = if proc < 0 { errno() } else { 0i32 };
    if ready == 0 && unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } < 0 {
        ready = errno();
    }
    if !unsafe { transfer(fd, (&mut ready as *mut i32).cast(), 4, true) } || ready != 0 {
        unsafe {
            libc::_exit(1);
        }
    }
    let mut generation = 0u64;
    let mut last_reap = 0;
    loop {
        let mut tick: libc::timespec = unsafe { std::mem::zeroed() };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut tick);
        }
        if tick.tv_sec > last_reap {
            unsafe {
                reap_orphans(proc, sessions);
            }
            last_reap = tick.tv_sec;
        }
        // A pidfd also detects actual owner death if an application fork
        // retained a duplicate of its close-on-exec ownership socket.
        let mut polls = [
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: owner_identity,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(polls.as_mut_ptr(), 2, 1000) };
        if ready == 0 || (ready < 0 && errno() == libc::EINTR) {
            continue;
        }
        if ready < 0 || polls[1].revents != 0 {
            break;
        }
        let mut header = [0u64; 6];
        let Some(master) = (unsafe { receive_header(fd, &mut header) }) else {
            break;
        };
        let mut reply: Reply = [0, 0, 0, -1];
        if header[0] == SPAWN {
            let len = header[3] as usize;
            let strings = header[4]
                .checked_add(header[5])
                .and_then(|count| count.checked_add(2))
                .filter(|count| *count <= MAX_STRINGS as u64);
            if master < 0 || len > payload.len() || header[4] == 0 || strings.is_none() {
                if master >= 0 {
                    unsafe {
                        libc::close(master);
                    }
                }
                break;
            }
            if !unsafe { transfer(fd, payload.as_mut_ptr(), len, false) } {
                unsafe {
                    libc::close(master);
                }
                break;
            }
            let strings = strings.unwrap_or(0) as usize;
            let mut offset = 0;
            let mut valid = true;
            for pointer in pointers.iter_mut().take(strings) {
                *pointer = unsafe { payload.as_ptr().add(offset).cast() };
                let Some(end) = payload[offset..len].iter().position(|byte| *byte == 0) else {
                    valid = false;
                    break;
                };
                offset += end + 1;
            }
            if !valid || offset != len {
                unsafe {
                    libc::close(master);
                }
                break;
            }
            // Insert argv/envp terminators into the preallocated pointer table.
            let env_start = 2 + header[4] as usize;
            pointers.copy_within(env_start..strings, env_start + 1);
            pointers[env_start] = std::ptr::null();
            pointers[strings + 1] = std::ptr::null();
            if let Some(index) = sessions.iter().position(|slot| slot.pid == 0) {
                generation += 1;
                let (pid, error) = unsafe {
                    spawn_child(
                        fd,
                        owner_identity,
                        master,
                        pointers,
                        env_start + 1,
                        descriptors,
                    )
                };
                unsafe {
                    libc::close(master);
                }
                if pid > 0 {
                    sessions[index] = Session { pid, generation };
                    if error != 0 {
                        unsafe {
                            libc::kill(pid, libc::SIGKILL);
                            clean_session(proc, pid);
                        }
                        if unsafe { reap_leader(pid) } {
                            sessions[index] = Session::default();
                        }
                        unsafe {
                            reap_orphans(proc, sessions);
                        }
                    } else {
                        reply[1] = generation as i64;
                        reply[2] = pid as i64;
                    }
                }
                reply[0] = error as i64;
            } else {
                unsafe {
                    libc::close(master);
                }
                reply[0] = libc::ENOSPC as i64;
            }
        } else {
            if master >= 0 {
                unsafe {
                    libc::close(master);
                }
                break;
            }
            if header[3] != 0 {
                break;
            }
            if let Some(index) = sessions.iter().position(|slot| {
                slot.pid as u64 == header[2] && slot.generation == header[1] && slot.pid > 0
            }) {
                let pid = sessions[index].pid;
                match header[0] {
                    STATUS => {
                        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                        if unsafe {
                            libc::waitid(
                                libc::P_PID,
                                pid as u32,
                                &mut info,
                                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                            )
                        } < 0
                        {
                            reply[0] = errno() as i64;
                        } else if unsafe { info.si_pid() } != 0 {
                            let status = unsafe { info.si_status() };
                            reply[3] = if info.si_code == libc::CLD_EXITED {
                                status << 8
                            } else {
                                status
                            } as i64;
                        }
                    }
                    KILL_LEADER => unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    },
                    SIGNAL
                        if header[4] == libc::SIGHUP as u64
                            || header[4] == libc::SIGKILL as u64 =>
                    {
                        if header[4] == libc::SIGKILL as u64 {
                            unsafe {
                                clean_session(proc, pid);
                            }
                        } else {
                            unsafe {
                                visit_session(proc, pid, libc::SIGHUP);
                            }
                        }
                    }
                    RELEASE => {
                        unsafe {
                            clean_session(proc, pid);
                        }
                        if unsafe { reap_leader(pid) } {
                            sessions[index] = Session::default();
                        } else {
                            reply[0] = libc::ETIMEDOUT as i64;
                        }
                        unsafe {
                            reap_orphans(proc, sessions);
                        }
                    }
                    _ => reply[0] = libc::EINVAL as i64,
                }
            } else {
                reply[0] = libc::ESRCH as i64;
            }
        }
        if !unsafe { transfer(fd, reply.as_mut_ptr().cast(), size_of::<Reply>(), true) } {
            break;
        }
    }
    // Signal all sessions before waiting for any one unresponsive process.
    unsafe {
        clean_sessions(proc, sessions);
    }
    for _ in 0..100 {
        unsafe {
            reap_orphans(proc, &[]);
        }
        let mut live = false;
        for session in sessions.iter_mut().filter(|slot| slot.pid > 0) {
            let result = unsafe { libc::waitpid(session.pid, std::ptr::null_mut(), libc::WNOHANG) };
            if result == 0 {
                live = true;
            } else {
                *session = Session::default();
            }
        }
        if !live {
            break;
        }
        unsafe {
            libc::poll(std::ptr::null_mut(), 0, 2);
        }
    }
    unsafe {
        libc::_exit(0);
    }
}

unsafe fn spawn_child(
    owner: i32,
    owner_identity: i32,
    master: i32,
    pointers: &mut [*const libc::c_char],
    env_start: usize,
    descriptors: i32,
) -> (i32, i32) {
    let slave = unsafe {
        libc::ioctl(
            master,
            libc::TIOCGPTPEER,
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return (-1, errno());
    }
    let mut status = [0i32; 2];
    if unsafe { libc::pipe2(status.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        let error = errno();
        unsafe {
            libc::close(slave);
        }
        return (-1, error);
    }
    let pid = unsafe { fork_process(std::ptr::null_mut()) };
    if pid == 0 {
        unsafe {
            libc::close(status[0]);
            // All descriptors here are helper-owned. Install stdio before
            // closing everything except the exec-error pipe.
            let mut error = 0;
            if libc::setsid() < 0 {
                error = errno();
            }
            for target in 0..3 {
                if libc::dup2(slave, target) < 0 {
                    error = errno();
                }
            }
            if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                error = errno();
            }
            if libc::chdir(pointers[1]) < 0 {
                error = errno();
            }
            // All signal dispositions must be reset, including a host's
            // ignored SIGCHLD, before unblocking signals in the PTY child.
            for signal in 1..65 {
                if signal != libc::SIGKILL && signal != libc::SIGSTOP && !reset_signal(signal) {
                    error = errno();
                }
            }
            let mut empty = std::mem::zeroed();
            libc::sigemptyset(&mut empty);
            libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
            // Stdio remains open; every other inherited FD is closed.
            if libc::syscall(libc::SYS_close_range, 3u32, status[1] as u32 - 1, 0) < 0 {
                for fd in 3..status[1] {
                    libc::close(fd);
                }
            }
            if libc::syscall(libc::SYS_close_range, status[1] as u32 + 1, u32::MAX, 0) < 0 {
                for fd in status[1] + 1..descriptors {
                    libc::close(fd);
                }
            }
            if error == 0 {
                libc::execve(
                    pointers[0],
                    pointers.as_ptr().add(2),
                    pointers.as_ptr().add(env_start),
                );
                error = errno();
                // Match execvp's behavior for executable text without a shebang.
                if error == libc::ENOEXEC {
                    pointers[1] = c"/bin/sh".as_ptr();
                    pointers[2] = pointers[0];
                    libc::execve(
                        pointers[1],
                        pointers.as_ptr().add(1),
                        pointers.as_ptr().add(env_start),
                    );
                    error = errno();
                }
            }
            libc::write(status[1], (&error as *const i32).cast(), 4);
            libc::_exit(127);
        }
    }
    let fork_error = if pid < 0 { errno() } else { 0 };
    unsafe {
        libc::close(slave);
        libc::close(status[1]);
    }
    if pid < 0 {
        unsafe {
            libc::close(status[0]);
        }
        return (pid, fork_error);
    }
    // The child already belongs to this broker, including during this wait.
    // Bound startup and notice owner death even if exec stalls on a filesystem.
    let mut polls = [
        libc::pollfd {
            fd: status[0],
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        },
        libc::pollfd {
            fd: owner,
            events: libc::POLLRDHUP,
            revents: 0,
        },
        libc::pollfd {
            fd: owner_identity,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ready = unsafe { libc::poll(polls.as_mut_ptr(), 3, 4000) };
    let mut error = 0i32;
    if ready <= 0 {
        error = if ready == 0 { libc::ETIMEDOUT } else { errno() };
    } else if polls[1].revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR) != 0
        || polls[2].revents != 0
    {
        error = libc::EPIPE;
    } else {
        let n = unsafe { libc::read(status[0], (&mut error as *mut i32).cast(), 4) };
        if n != 0 && n != 4 {
            error = libc::EIO;
        }
    }
    unsafe {
        libc::close(status[0]);
    }
    (pid, error)
}

// /proc getdents64 with a fixed buffer avoids libc opendir/readdir allocation.
unsafe fn visit_processes(proc: i32, mut action: impl FnMut(i32)) {
    let mut entries = [0u8; 32768];
    unsafe {
        libc::lseek(proc, 0, libc::SEEK_SET);
    }
    loop {
        let n = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                proc,
                entries.as_mut_ptr(),
                entries.len(),
            )
        };
        if n <= 0 {
            break;
        }
        let mut offset = 0;
        while offset + 19 < n as usize {
            let length = u16::from_ne_bytes([entries[offset + 16], entries[offset + 17]]) as usize;
            if length < 20 || offset + length > n as usize {
                break;
            }
            let mut pid = 0i32;
            for &byte in &entries[offset + 19..offset + length] {
                if byte == 0 {
                    break;
                }
                if !byte.is_ascii_digit() {
                    pid = 0;
                    break;
                }
                pid = pid
                    .checked_mul(10)
                    .and_then(|value| value.checked_add((byte - b'0') as i32))
                    .unwrap_or_default();
            }
            offset += length;
            if pid > 0 {
                action(pid);
            }
        }
    }
}
unsafe fn signal_sessions(proc: i32, sessions: &[Session], signal: i32) {
    unsafe {
        visit_processes(proc, |pid| {
            let session = libc::getsid(pid);
            if session <= 0 || !sessions.iter().any(|slot| slot.pid == session) {
                return;
            }
            let handle = libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32;
            if handle < 0 {
                return;
            }
            // PID cannot name another session incarnation: its original leader
            // remains our unreaped child. The pidfd pins the exact signal target.
            if libc::getsid(pid) == session {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    handle,
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
                // A process that crossed the deliberate setsid boundary just before
                // the stopping signal must not be left stopped by our inventory.
                if signal == libc::SIGSTOP && libc::getsid(pid) != session {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        handle,
                        libc::SIGCONT,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    );
                }
            }
            libc::close(handle);
        });
    }
}
unsafe fn visit_session(proc: i32, session: i32, signal: i32) {
    unsafe {
        signal_sessions(
            proc,
            &[Session {
                pid: session,
                generation: 0,
            }],
            signal,
        );
    }
}
unsafe fn clean_sessions(proc: i32, sessions: &[Session]) {
    // Stop forkers before the killing scan. Repeat to catch a new child which
    // appeared behind getdents' cursor; stopped processes cannot create more.
    for _ in 0..2 {
        unsafe {
            signal_sessions(proc, sessions, libc::SIGSTOP);
        }
    }
    for _ in 0..2 {
        unsafe {
            signal_sessions(proc, sessions, libc::SIGKILL);
            libc::poll(std::ptr::null_mut(), 0, 5);
        }
    }
}
unsafe fn clean_session(proc: i32, session: i32) {
    unsafe {
        clean_sessions(
            proc,
            &[Session {
                pid: session,
                generation: 0,
            }],
        );
    }
}
unsafe fn reap_leader(pid: i32) -> bool {
    for _ in 0..100 {
        let result = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        if result == pid || (result < 0 && errno() == libc::ECHILD) {
            return true;
        }
        unsafe {
            libc::poll(std::ptr::null_mut(), 0, 2);
        }
    }
    false
}
unsafe fn reap_orphans(proc: i32, sessions: &[Session]) {
    // Only actual children can be reaped. Registered leaders retain their PID
    // reservations; adopted descendants (including exited setsid escapees) do
    // not accumulate as zombies in a long-running standalone broker.
    unsafe {
        visit_processes(proc, |pid| {
            if !sessions.iter().any(|slot| slot.pid == pid) {
                libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portable_pty::{PtyPair, PtySize, native_pty_system};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "tx-broker-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn pair() -> PtyPair {
        native_pty_system().openpty(PtySize::default()).unwrap()
    }
    fn command(program: impl AsRef<OsStr>, args: &[&str]) -> CommandBuilder {
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
        command.cwd("/");
        command
    }
    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "Linux broker fixture did not converge"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn exists(pid: i32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    fn children(pid: i32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default()
    }

    #[test]
    fn linux_watchdog_rejects_stale_identities_and_failed_registrations() {
        let watchdog = Watchdog::start().unwrap();
        let pty = pair();
        let (mut child, registration) = watchdog
            .spawn(
                command("/bin/sh", &["-c", "exit 7"]),
                pty.master.as_raw_fd().unwrap(),
            )
            .unwrap();
        let pid = child.process_id().unwrap() as i32;
        let mut status = None;
        wait_until(|| {
            status = child.try_wait().unwrap();
            status.is_some()
        });
        assert_eq!(status.unwrap().exit_code(), 7);
        assert!(
            exists(pid),
            "the exited leader must reserve its session's PID"
        );
        assert!(
            children(watchdog.pid)
                .split_whitespace()
                .any(|id| id == pid.to_string())
        );
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            errno(),
            libc::ECHILD,
            "the watchdog, not the embedding process, owns the PTY child"
        );
        let generation = registration.0.generation;
        for operation in [RELEASE, SIGNAL, KILL_LEADER, STATUS] {
            let error = watchdog
                .request(
                    [
                        operation,
                        generation + 1,
                        pid as u64,
                        0,
                        libc::SIGKILL as u64,
                        0,
                    ],
                    &[],
                    None,
                )
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
            assert!(
                exists(pid),
                "a stale registration must not release the PID reservation"
            );
        }
        // Registered generation and PID are both required. Arbitrary processes
        // cannot be submitted for cleanup through this private broker protocol.
        let error = watchdog
            .request(
                [KILL_LEADER, generation, std::process::id() as u64, 0, 0, 0],
                &[],
                None,
            )
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
        drop(child);
        assert!(
            exists(pid),
            "the pane registration outlives its reaper handle"
        );
        drop(registration);
        wait_until(|| !exists(pid));
        assert_eq!(
            watchdog
                .request([STATUS, generation, pid as u64, 0, 0, 0], &[], None)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn linux_broker_exec_failure_and_descriptor_rejection_leave_no_children() {
        let watchdog = Watchdog::start().unwrap();
        let directory = Directory::new();
        let pty = pair();
        let bad = directory.0.join("missing-interpreter");
        std::fs::write(&bad, b"#!/tmuxx-no-such-interpreter\n").unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            watchdog
                .spawn(command(&bad, &[]), pty.master.as_raw_fd().unwrap())
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        wait_until(|| children(watchdog.pid).trim().is_empty());
        let invalid = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(
            watchdog
                .spawn(command("/bin/true", &[]), invalid.as_raw_fd())
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOTTY)
        );
        assert!(children(watchdog.pid).trim().is_empty());
        let mut invalid_command = command("/bin/true", &[]);
        invalid_command.arg(OsStr::from_bytes(b"bad\0argument"));
        assert_eq!(
            watchdog
                .spawn(invalid_command, pty.master.as_raw_fd().unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let mut too_large = command("/bin/true", &[]);
        too_large.arg("x".repeat(MAX_BYTES));
        assert_eq!(
            watchdog
                .spawn(too_large, pty.master.as_raw_fd().unwrap())
                .unwrap_err()
                .raw_os_error(),
            Some(libc::E2BIG)
        );
        let text = directory.0.join("executable-text");
        std::fs::write(&text, b"exit 23\n").unwrap();
        std::fs::set_permissions(&text, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (mut child, registration) = watchdog
            .spawn(command(&text, &[]), pty.master.as_raw_fd().unwrap())
            .unwrap();
        let mut status = None;
        wait_until(|| {
            status = child.try_wait().unwrap();
            status.is_some()
        });
        assert_eq!(
            status.unwrap().exit_code(),
            23,
            "execvp text-script fallback is retained"
        );
        drop(child);
        drop(registration);
        wait_until(|| children(watchdog.pid).trim().is_empty());
    }

    #[test]
    fn linux_broker_lost_spawn_reply_cleans_nested_orphans_before_exit() {
        let watchdog = Watchdog::start().unwrap();
        let independent = Watchdog::start().unwrap();
        let independent_pty = pair();
        let (mut other, other_registration) = independent
            .spawn(
                command("/bin/sleep", &["60"]),
                independent_pty.master.as_raw_fd().unwrap(),
            )
            .unwrap();
        let directory = Directory::new();
        let pty = pair();
        let path = directory.0.join("pids");
        let script = "import os,signal,sys,time\nsignal.signal(signal.SIGHUP,signal.SIG_IGN)\nr,w=os.pipe()\nchild=os.fork()\nif child==0:\n os.close(r); os.setpgid(0,0); nested=os.fork()\n if nested==0: os.close(w); time.sleep(60); os._exit(0)\n os.write(w,str(nested).encode()); os.close(w); os._exit(0)\nos.close(w); nested=int(os.read(r,100)); os.close(r); os.waitpid(child,0)\nopen(sys.argv[1],'w').write(str(os.getpid())+' '+str(nested))\ntime.sleep(60)";
        let args = [
            "/usr/bin/python3",
            "-u",
            "-c",
            script,
            path.to_str().unwrap(),
        ];
        let mut payload = Vec::new();
        for value in ["/usr/bin/python3", "/"]
            .into_iter()
            .chain(args)
            .chain(["PATH=/usr/bin:/bin"])
        {
            payload.extend_from_slice(value.as_bytes());
            payload.push(0);
        }
        let mut header = [SPAWN, 0, 0, payload.len() as u64, args.len() as u64, 1];
        {
            let guard = watchdog.socket.lock().unwrap();
            let socket = guard.as_ref().unwrap();
            write_request(
                socket.as_raw_fd(),
                &mut header,
                &payload,
                pty.master.as_raw_fd(),
            )
            .unwrap();
            // Intentionally never read the successful spawn reply. Ownership
            // must already exist even though the host never accepted a PID.
        }
        wait_until(|| {
            std::fs::read_to_string(&path).is_ok_and(|s| s.split_whitespace().count() == 2)
        });
        let pids = std::fs::read_to_string(&path)
            .unwrap()
            .split_whitespace()
            .map(|s| s.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert!(pids.iter().all(|pid| exists(*pid)));
        let broker_pid = watchdog.pid;
        drop(watchdog);
        wait_until(|| pids.iter().all(|pid| !exists(*pid)));
        assert!(!exists(broker_pid));
        assert!(other.try_wait().unwrap().is_none());
        drop(other);
        drop(other_registration);
    }

    #[test]
    fn linux_embedded_host_process_kill_cleans_owned_panes() {
        const CHILD_ENV: &str = "TMUXX_TEST_LINUX_EMBEDDED_OWNER";
        if let Some(directory) = std::env::var_os(CHILD_ENV) {
            use tmuxx_protocol::native::{Request, ServerMessage};
            let directory = PathBuf::from(directory);
            let host = crate::start(crate::Options::new(directory.join("native"))).unwrap();
            let client = host.connect().unwrap();
            let script = "import os,signal,sys,time\nsignal.signal(signal.SIGHUP,signal.SIG_IGN)\nopen(sys.argv[1],'w').write(str(os.getpid()))\ntime.sleep(60)";
            let path = directory.join("pane-pid");
            client
                .send(Request {
                    request_id: 1,
                    argv: [
                        "new-session",
                        "-d",
                        "-s",
                        "embedded",
                        "/usr/bin/python3",
                        "-c",
                        script,
                        path.to_str().unwrap(),
                    ]
                    .into_iter()
                    .map(|s| s.as_bytes().to_vec())
                    .collect(),
                    stdin: None,
                    subscribe: false,
                    control: false,
                })
                .unwrap();
            let mut reply = None;
            wait_until(|| {
                if let Ok(ServerMessage::Reply(response)) = client.try_recv() {
                    reply = Some(response);
                }
                reply.is_some()
            });
            assert_eq!(reply.unwrap().error, None);
            std::fs::write(directory.join("acknowledged"), "ready").unwrap();
            // This process truly embeds the runtime. The parent kills it and
            // prevents all ordinary Rust destructors from running.
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        struct Owner(std::process::Child);
        impl Drop for Owner {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let directory = Directory::new();
        let mut owner = Owner(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "watchdog::tests::linux_embedded_host_process_kill_cleans_owned_panes",
                    "--nocapture",
                ])
                .env(CHILD_ENV, &directory.0)
                .stdin(std::process::Stdio::null())
                // Keep the nested harness banner out of the parent's exact
                // result line. Panic diagnostics remain visible on stderr.
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait_until(|| {
            assert!(
                owner.0.try_wait().unwrap().is_none(),
                "embedded host exited before startup"
            );
            directory.0.join("acknowledged").exists()
                && std::fs::read_to_string(directory.0.join("pane-pid"))
                    .is_ok_and(|s| !s.is_empty())
        });
        let pane: i32 = std::fs::read_to_string(directory.0.join("pane-pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(exists(pane));
        owner.0.kill().unwrap();
        assert_eq!(owner.0.wait().unwrap().signal(), Some(libc::SIGKILL));
        wait_until(|| !exists(pane));
        assert!(UnixStream::connect(directory.0.join("native")).is_err());
    }

    #[test]
    fn linux_broker_malformed_spawn_header_cleans_registered_children() {
        let watchdog = Watchdog::start().unwrap();
        let pty = pair();
        let (child, registration) = watchdog
            .spawn(
                command("/bin/sleep", &["60"]),
                pty.master.as_raw_fd().unwrap(),
            )
            .unwrap();
        let pid = child.process_id().unwrap() as i32;
        let error = watchdog
            .request(
                [SPAWN, 0, 0, 0, u64::MAX, u64::MAX],
                &[],
                pty.master.as_raw_fd(),
            )
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
        ));
        wait_until(|| !exists(pid));
        // A second failed send must be an ordinary error, never SIGPIPE or a
        // retry that could duplicate the originally accepted spawn.
        assert!(
            watchdog
                .request([STATUS, 0, 0, 0, 0, 0], &[], None)
                .is_err()
        );
        drop(child);
        drop(registration);
    }

    #[test]
    fn linux_owner_pidfd_cleans_panes_despite_inherited_socket() {
        const CHILD_ENV: &str = "TMUXX_TEST_LINUX_FORK_OWNER";
        if let Some(directory) = std::env::var_os(CHILD_ENV) {
            let directory = PathBuf::from(directory);
            // Host signal dispositions are process-wide: exercise them only
            // in this separately launched fixture, never in the test harness.
            unsafe {
                libc::signal(libc::SIGCHLD, libc::SIG_IGN);
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            }
            let automatic_reap = Watchdog::start().unwrap();
            let automatic_pid = automatic_reap.pid;
            drop(automatic_reap);
            assert!(
                !exists(automatic_pid),
                "Drop must wait for actual pidfd exit even with SIGCHLD ignored"
            );
            let failed = Watchdog::start().unwrap();
            signal_pidfd(failed.identity.as_raw_fd(), libc::SIGKILL);
            assert!(wait_broker(
                failed.identity.as_raw_fd(),
                Duration::from_secs(1)
            ));
            assert!(
                failed.request([STATUS, 0, 0, 0, 0, 0], &[], None).is_err(),
                "dead broker writes must return errors without SIGPIPE"
            );
            drop(failed);
            let watchdog = Watchdog::start().unwrap();
            let pty = pair();
            let marker = directory.join("hup-ignored");
            let (child, _registration) = watchdog
                .spawn(
                    command(
                        "/bin/sh",
                        &[
                            "-c",
                            "trap '' HUP; printf ready > \"$1\"; exec sleep 60",
                            "sh",
                            marker.to_str().unwrap(),
                        ],
                    ),
                    pty.master.as_raw_fd().unwrap(),
                )
                .unwrap();
            let fd = watchdog
                .socket
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_raw_fd();
            let descriptors = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) } as i32;
            let keeper = unsafe { fork_process(std::ptr::null_mut()) };
            if keeper == 0 {
                unsafe {
                    close_except(fd, -1, descriptors);
                    libc::setsid();
                    // Deliberately keep the ownership socket after fork without
                    // exec. Its CLOEXEC bit does not help until a future exec.
                    libc::poll(std::ptr::null_mut(), 0, 60_000);
                    libc::_exit(0);
                }
            }
            assert!(keeper > 0);
            std::fs::write(
                directory.join("pids"),
                format!("{} {}", child.process_id().unwrap(), keeper),
            )
            .unwrap();
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        struct Owner(std::process::Child);
        impl Drop for Owner {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct Keeper(OwnedFd);
        impl Drop for Keeper {
            fn drop(&mut self) {
                signal_pidfd(self.0.as_raw_fd(), libc::SIGKILL);
            }
        }
        let directory = Directory::new();
        let mut owner = Owner(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "watchdog::tests::linux_owner_pidfd_cleans_panes_despite_inherited_socket",
                    "--nocapture",
                ])
                .env(CHILD_ENV, &directory.0)
                .stdin(std::process::Stdio::null())
                // Keep the nested harness banner out of the parent's exact
                // result line. Panic diagnostics remain visible on stderr.
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        wait_until(|| {
            assert!(owner.0.try_wait().unwrap().is_none());
            std::fs::read_to_string(directory.0.join("pids"))
                .is_ok_and(|s| s.split_whitespace().count() == 2)
        });
        let ids = std::fs::read_to_string(directory.0.join("pids"))
            .unwrap()
            .split_whitespace()
            .map(|s| s.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        let handle = unsafe { libc::syscall(libc::SYS_pidfd_open, ids[1], 0) } as i32;
        assert!(handle >= 0);
        let keeper = Keeper(unsafe { OwnedFd::from_raw_fd(handle) });
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        wait_until(|| !exists(ids[0]));
        let mut poll = libc::pollfd {
            fd: keeper.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut poll, 1, 0) },
            0,
            "unrelated worker must survive owner cleanup"
        );
        drop(keeper);
        wait_until(|| !exists(ids[1]));
    }

    #[test]
    fn linux_broker_preserves_pty_output_bytes() {
        let watchdog = Watchdog::start().unwrap();
        let pty = pair();
        let mut reader = pty.master.try_clone_reader().unwrap();
        let master = pty.master.as_raw_fd().unwrap();
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        assert_eq!(
            unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let (child, registration) = watchdog
            .spawn(
                command("/bin/sh", &["-c", "printf 'LINE1\\nREADY'; sleep 60"]),
                master,
            )
            .unwrap();
        let mut data = Vec::new();
        wait_until(|| {
            let mut bytes = [0u8; 256];
            match reader.read(&mut bytes) {
                Ok(n) => data.extend_from_slice(&bytes[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
            data.ends_with(b"READY")
        });
        assert_eq!(data, b"LINE1\r\nREADY");
        drop(child);
        drop(registration);
    }
}
