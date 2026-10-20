//! macOS has no parent-death signal. A descriptor-owning helper cleans owned
//! PTY sessions if the embedding application disappears without running Drop.
use std::{
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_SESSIONS: usize = 16_384;
const MAX_PIDS: usize = 131_072;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Session {
    pid: libc::pid_t,
    padding: u32,
    seconds: u64,
    micros: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Message {
    operation: u64,
    session: Session,
}

pub(super) struct Watchdog {
    socket: Mutex<Option<UnixStream>>,
    pid: libc::pid_t,
}
pub(super) struct Registration {
    watchdog: Arc<Watchdog>,
    session: Session,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let _ = self.watchdog.send(2, self.session);
    }
}
impl Watchdog {
    pub(super) fn start() -> io::Result<Arc<Self>> {
        let (mut parent, child) = UnixStream::pair()?;
        parent.set_read_timeout(Some(Duration::from_secs(5)))?;
        parent.set_write_timeout(Some(Duration::from_secs(5)))?;
        let enabled: libc::c_int = 1;
        for socket in [&parent, &child] {
            // A dead watchdog must fail a write, never deliver SIGPIPE to the app.
            if unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_NOSIGPIPE,
                    (&enabled as *const libc::c_int).cast(),
                    size_of_val(&enabled) as libc::socklen_t,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let mut sessions = vec![Session::default(); MAX_SESSIONS];
        let mut pids = vec![0; MAX_PIDS];
        // Everything the child needs is allocated before fork. It never enters
        // the Rust allocator, locks, std I/O or destructors after the fork.
        let descriptors = unsafe { libc::getdtablesize() };
        let descriptor = child.as_raw_fd();
        // An inherited application signal handler could allocate or lock in
        // the child. It needs only socket EOF, so leave its signals blocked.
        let mut blocked = unsafe { std::mem::zeroed() };
        let mut original = unsafe { std::mem::zeroed() };
        let error = unsafe {
            libc::sigfillset(&mut blocked);
            libc::pthread_sigmask(libc::SIG_SETMASK, &blocked, &mut original)
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe {
                if libc::setsid() < 0 {
                    libc::_exit(1);
                }
                for fd in 0..descriptors {
                    if fd != descriptor {
                        libc::close(fd);
                    }
                }
                child_loop(descriptor, &mut sessions, &mut pids);
            }
        }
        let error = (pid < 0).then(io::Error::last_os_error);
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &original, std::ptr::null_mut()) };
        if let Some(error) = error {
            return Err(error);
        }
        drop(child);
        let mut ready = [0; 4];
        if let Err(error) = parent.read_exact(&mut ready) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
            return Err(error);
        }
        Ok(Arc::new(Self {
            socket: Mutex::new(Some(parent)),
            pid,
        }))
    }

    pub(super) fn register(self: &Arc<Self>, pid: u32) -> io::Result<Registration> {
        let session = identity(pid as libc::pid_t)
            .ok_or_else(|| io::Error::other("cannot identify owned PTY session"))?;
        if unsafe { libc::getsid(session.pid) } != session.pid {
            return Err(io::Error::other(
                "PTY child did not create an owned session",
            ));
        }
        self.send(1, session)?;
        Ok(Registration {
            watchdog: self.clone(),
            session,
        })
    }

    fn send(&self, operation: u64, session: Session) -> io::Result<()> {
        let message = Message { operation, session };
        // The local fixed-width message has no uninitialized padding.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&message as *const Message).cast::<u8>(),
                size_of::<Message>(),
            )
        };
        let mut socket = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let socket = socket
            .as_mut()
            .ok_or_else(|| io::Error::other("watchdog closed"))?;
        socket.write_all(bytes)?;
        let mut result = [0; 4];
        socket.read_exact(&mut result)?;
        let error = i32::from_ne_bytes(result);
        if error == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error))
        }
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.socket
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let result = unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), libc::WNOHANG) };
            if result == self.pid || result < 0 {
                break;
            }
            if Instant::now() >= deadline {
                unsafe {
                    libc::kill(self.pid, libc::SIGKILL);
                    libc::waitpid(self.pid, std::ptr::null_mut(), 0);
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn identity(pid: libc::pid_t) -> Option<Session> {
    // Darwin libproc's PID query is a direct proc_info syscall wrapper. The
    // fixed BSD-info structure avoids allocations and inherited runtime locks.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of_val(&info) as libc::c_int;
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    } != size
    {
        return None;
    }
    Some(Session {
        pid,
        padding: 0,
        seconds: info.pbi_start_tvsec,
        micros: info.pbi_start_tvusec,
    })
}

// Post-fork code uses only fixed buffers, pure arithmetic and libc syscall wrappers.
unsafe fn child_loop(fd: libc::c_int, sessions: &mut [Session], pids: &mut [libc::pid_t]) -> ! {
    let _ = unsafe { transfer(fd, (&mut 0i32 as *mut i32).cast(), 4, true) };
    loop {
        let mut message = Message::default();
        if !unsafe {
            transfer(
                fd,
                (&mut message as *mut Message).cast(),
                size_of::<Message>(),
                false,
            )
        } {
            break;
        }
        let mut result: i32 = 0;
        match message.operation {
            1 => {
                if let Some(slot) = sessions
                    .iter_mut()
                    .find(|entry| entry.pid == message.session.pid || entry.pid == 0)
                {
                    *slot = message.session;
                } else {
                    result = libc::ENOSPC;
                }
            }
            2 => {
                for slot in &mut *sessions {
                    if *slot == message.session {
                        *slot = Session::default();
                    }
                }
            }
            _ => result = libc::EINVAL,
        }
        if !unsafe { transfer(fd, (&mut result as *mut i32).cast(), 4, true) } {
            break;
        }
    }
    for session in &mut *sessions {
        if session.pid > 0 && identity(session.pid).is_some_and(|current| current != *session) {
            *session = Session::default();
        }
    }
    if sessions.iter().any(|session| session.pid > 0) {
        let own_group = unsafe { libc::getpgrp() };
        for pass in 0..2 {
            let count = unsafe {
                libc::proc_listallpids(pids.as_mut_ptr().cast(), size_of_val(pids) as libc::c_int)
            }
            .max(0) as usize;
            for &pid in pids.iter().take(count.min(pids.len())) {
                if pid <= 0 {
                    continue;
                }
                let session = unsafe { libc::getsid(pid) };
                if session > 0 && sessions.iter().any(|entry| entry.pid == session) {
                    let group = unsafe { libc::getpgid(pid) };
                    if group > 0 && group != own_group {
                        unsafe {
                            libc::kill(-group, libc::SIGKILL);
                        }
                    }
                }
            }
            // The first pass stops the forkers; the second catches jobs forked
            // while its PID inventory was being collected.
            if pass == 0 {
                unsafe {
                    libc::poll(std::ptr::null_mut(), 0, 20);
                }
            }
        }
    }
    unsafe { libc::_exit(0) }
}

unsafe fn transfer(fd: libc::c_int, mut bytes: *mut u8, mut length: usize, write: bool) -> bool {
    while length != 0 {
        let result = if write {
            unsafe { libc::write(fd, bytes.cast(), length) }
        } else {
            unsafe { libc::read(fd, bytes.cast(), length) }
        };
        if result < 0 && unsafe { *libc::__error() } == libc::EINTR {
            continue;
        }
        if result <= 0 {
            return false;
        }
        bytes = unsafe { bytes.add(result as usize) };
        length -= result as usize;
    }
    true
}
