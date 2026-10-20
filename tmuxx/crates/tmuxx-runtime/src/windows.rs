//! Owner-private local named pipes. Discovery is public metadata, never a credential.
//!
//! Pipe names include the account SID, not the logon SID: desktop and SSH logons
//! of the same account share a namespace. Both ends verify token identity.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{IO::*, Pipes::*, Threading::*},
};

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in Windows path",
        ));
    }
    value.push(0);
    Ok(value)
}

fn handle(value: HANDLE) -> io::Result<OwnedHandle> {
    if value.is_null() || value == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful Win32 handle creation transfers exactly one owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(value) })
}

fn token_sid(token: &OwnedHandle) -> io::Result<String> {
    let mut size = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut size,
        );
    }
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    // TOKEN_USER requires pointer alignment; Vec<u8> would not provide it.
    let mut storage = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(text, len))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid account SID"));
        LocalFree(text.cast());
        result
    }
}

fn process_sid(process: HANDLE) -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    token_sid(&handle(token)?)
}

/// The process account identity, independent of its desktop/SSH logon session.
pub fn account_sid() -> io::Result<String> {
    process_sid(unsafe { GetCurrentProcess() })
}

fn prefix() -> io::Result<String> {
    Ok(format!(r"\\.\pipe\tmuxx-{}-", account_sid()?))
}

/// Encode instance names injectively; Unicode and shell metacharacters are data.
pub fn endpoint(instance: &str) -> io::Result<PathBuf> {
    if instance.is_empty() || instance.len() > 64 || instance.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "instance name must contain 1..64 UTF-8 bytes without NUL",
        ));
    }
    let encoded: String = instance
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(PathBuf::from(format!("{}{encoded}", prefix()?)))
}

fn validate_endpoint(path: &Path) -> io::Result<()> {
    let value = path.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "pipe endpoint must be Unicode")
    })?;
    let prefix = prefix()?;
    let suffix = value.strip_prefix(&prefix).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "endpoint must be a local tmuxx pipe for this account",
        )
    })?;
    if suffix.is_empty()
        || suffix.len() > 128
        || suffix.len() % 2 != 0
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid tmuxx pipe instance encoding",
        ));
    }
    Ok(())
}

struct Inner {
    handle: OwnedHandle,
    read_lock: Mutex<()>,
    write_lock: Mutex<()>,
    read_timeout: AtomicU64,
    write_timeout: AtomicU64,
    stopped: AtomicBool,
}

/// Binary duplex stream with cancellable, bounded overlapped I/O.
#[derive(Clone)]
pub struct PipeStream(Arc<Inner>);

impl PipeStream {
    fn new(handle: OwnedHandle) -> Self {
        Self(Arc::new(Inner {
            handle,
            read_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
            read_timeout: AtomicU64::new(u64::MAX),
            write_timeout: AtomicU64::new(10_000),
            stopped: AtomicBool::new(false),
        }))
    }
    fn raw(&self) -> HANDLE {
        self.0.handle.as_raw_handle()
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.read_timeout.store(
            timeout.map_or(u64::MAX, |t| {
                t.as_millis().max(1).min(u32::MAX as u128 - 1) as u64
            }),
            Ordering::Release,
        );
        Ok(())
    }
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.write_timeout.store(
            timeout.map_or(u64::MAX, |t| {
                t.as_millis().max(1).min(u32::MAX as u128 - 1) as u64
            }),
            Ordering::Release,
        );
        Ok(())
    }
    /// Named pipes have no OS half-close. Native peers use an explicit input EOF
    /// record, ordered after preceding requests, while retaining output delivery.
    pub fn shutdown(&self, direction: Shutdown) -> io::Result<()> {
        if direction == Shutdown::Write {
            return self.clone().write_all(b"{\"type\":\"end_of_input\"}\n");
        }
        self.0.stopped.store(true, Ordering::Release);
        unsafe {
            CancelIoEx(self.raw(), std::ptr::null());
        }
        Ok(())
    }
    fn transfer(&self, buffer: *mut u8, len: usize, write: bool) -> io::Result<usize> {
        if len == 0 {
            return Ok(0);
        }
        if self.0.stopped.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let event = handle(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) })?;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        let mut transferred = 0;
        let started = unsafe {
            if write {
                WriteFile(
                    self.raw(),
                    buffer.cast(),
                    len.min(u32::MAX as usize) as u32,
                    &mut transferred,
                    &mut overlapped,
                )
            } else {
                ReadFile(
                    self.raw(),
                    buffer.cast(),
                    len.min(u32::MAX as usize) as u32,
                    &mut transferred,
                    &mut overlapped,
                )
            }
        };
        if started == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return if write { Err(error) } else { Ok(0) };
            }
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            let timeout = if write {
                &self.0.write_timeout
            } else {
                &self.0.read_timeout
            }
            .load(Ordering::Acquire);
            // Shutdown can race between the initial stopped check and ReadFile.
            if self.0.stopped.load(Ordering::Acquire) {
                unsafe {
                    CancelIoEx(self.raw(), &overlapped);
                }
            }
            let wait = unsafe {
                WaitForSingleObject(event.as_raw_handle(), timeout.min(u32::MAX as u64) as u32)
            };
            if wait != WAIT_OBJECT_0 {
                unsafe {
                    CancelIoEx(self.raw(), &overlapped);
                    // The OVERLAPPED and buffer must remain alive until cancellation completes.
                    GetOverlappedResult(self.raw(), &overlapped, &mut transferred, 1);
                }
                return Err(if wait == WAIT_TIMEOUT {
                    io::ErrorKind::TimedOut.into()
                } else {
                    io::Error::last_os_error()
                });
            }
        }
        if unsafe { GetOverlappedResult(self.raw(), &overlapped, &mut transferred, 1) } == 0 {
            let error = io::Error::last_os_error();
            if !write && error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return Ok(0);
            }
            return Err(error);
        }
        Ok(transferred as usize)
    }
    /// Validate the impersonation token of the client that supplied the hello.
    pub(crate) fn verify_client(&self) -> io::Result<(u32, String, String)> {
        let expected = account_sid()?;
        let mut pid = 0;
        if unsafe { GetNamedPipeClientProcessId(self.raw(), &mut pid) } == 0 || pid == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { ImpersonateNamedPipeClient(self.raw()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                if unsafe { RevertToSelf() } == 0 {
                    std::process::abort();
                }
            }
        }
        let _revert = Revert;
        let mut token = std::ptr::null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if token_sid(&handle(token)?)? != expected {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // SECURITY_IDENTIFICATION permits SID verification but cannot perform
        // the account lookup used by GetUserNameW. The verified client and
        // owner SIDs match, so resolve that account only after reverting.
        drop(_revert);
        let user = super::defaults::user_name().ok_or_else(|| {
            io::Error::other("authenticated Windows client account name unavailable")
        })?;
        Ok((pid, expected, user))
    }
}

impl Read for PipeStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let _lock = self.0.read_lock.lock().unwrap();
        self.transfer(buffer.as_mut_ptr(), buffer.len(), false)
    }
}
impl Write for PipeStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let _lock = self.0.write_lock.lock().unwrap();
        self.transfer(buffer.as_ptr().cast_mut(), buffer.len(), true)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Connect only to an existing local owner. Never launches an owner or a pane.
pub fn connect(path: &Path) -> io::Result<PipeStream> {
    connect_timeout(path, Duration::from_secs(5))
}

fn connect_timeout(path: &Path, timeout: Duration) -> io::Result<PipeStream> {
    validate_endpoint(path)?;
    let name = wide(path.as_os_str())?;
    let deadline = std::time::Instant::now() + timeout;
    let pipe = loop {
        match handle(unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        }) {
            Ok(pipe) => break pipe,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                // The accept loop creates the next pipe instance after taking
                // the previous one. Concurrent/rapid clients can reach that
                // short gap. No handshake or command has been sent yet.
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "existing tmuxx instance remained busy during connection admission",
                    ));
                }
                let millis = remaining.as_millis().clamp(1, 100) as u32;
                if unsafe { WaitNamedPipeW(name.as_ptr(), millis) } == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(ERROR_SEM_TIMEOUT as i32) {
                        return Err(unavailable(error));
                    }
                }
            }
            Err(error) => return Err(unavailable(error)),
        }
    };
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
    if process_sid(process.as_raw_handle())? != account_sid()? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(PipeStream::new(pipe))
}

fn unavailable(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("existing tmuxx instance unavailable: {error}"),
    )
}

pub(crate) struct Listener {
    path: PathBuf,
    waiting: PipeStream,
}
impl Listener {
    pub(crate) fn bind(path: &Path) -> io::Result<Self> {
        validate_endpoint(path)?;
        Ok(Self {
            path: path.into(),
            waiting: Self::create(path, true)?,
        })
    }
    fn create(path: &Path, first: bool) -> io::Result<PipeStream> {
        let sid = account_sid()?;
        let sddl = wide(OsStr::new(&format!("O:{sid}D:P(A;;GA;;;{sid})")))?;
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let security = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let name = wide(path.as_os_str())?;
        let result = handle(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX
                    | FILE_FLAG_OVERLAPPED
                    | if first {
                        FILE_FLAG_FIRST_PIPE_INSTANCE
                    } else {
                        0
                    },
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                65_536,
                65_536,
                0,
                &security,
            )
        });
        unsafe {
            LocalFree(descriptor);
        }
        result.map(PipeStream::new)
    }
    pub(crate) fn accept(&mut self) -> io::Result<Option<PipeStream>> {
        let event = handle(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) })?;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        if unsafe { ConnectNamedPipe(self.waiting.raw(), &mut overlapped) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_IO_PENDING {
                let wait = unsafe { WaitForSingleObject(event.as_raw_handle(), 10) };
                let mut bytes = 0;
                if wait != WAIT_OBJECT_0 {
                    unsafe {
                        CancelIoEx(self.waiting.raw(), &overlapped);
                    }
                }
                let completed =
                    unsafe { GetOverlappedResult(self.waiting.raw(), &overlapped, &mut bytes, 1) };
                if completed == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
                        return Ok(None);
                    }
                    return Err(error);
                }
            } else if error != ERROR_PIPE_CONNECTED {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
        let next = Self::create(&self.path, false)?;
        Ok(Some(std::mem::replace(&mut self.waiting, next)))
    }
}

/// Enumerate currently live pipe names. Confirm incarnation through the hello;
/// entries can disappear or be replaced immediately after discovery.
pub fn discover() -> io::Result<Vec<PathBuf>> {
    let prefix = prefix()?;
    let pattern = wide(OsStr::new(&format!("{prefix}*")))?;
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let find = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
    if find == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(code) if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_NO_MORE_FILES as i32)
        {
            Ok(Vec::new())
        } else {
            Err(error)
        };
    }
    struct Find(HANDLE);
    impl Drop for Find {
        fn drop(&mut self) {
            unsafe {
                FindClose(self.0);
            }
        }
    }
    let _guard = Find(find);
    let mut found = Vec::new();
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(data.cFileName.len());
        let name = String::from_utf16(&data.cFileName[..len]).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "non-Unicode discovery entry")
        })?;
        let path = PathBuf::from(format!(r"\\.\pipe\{name}"));
        if validate_endpoint(&path).is_ok() {
            found.push(path);
        }
        if unsafe { FindNextFileW(find, &mut data) } == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_NO_MORE_FILES {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            break;
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn private_endpoint(tag: &str) -> PathBuf {
        endpoint(&format!("test-{}-{tag}", std::process::id())).unwrap()
    }

    fn accept(listener: &mut Listener) -> PipeStream {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(stream) = listener.accept().unwrap() {
                return stream;
            }
            assert!(
                Instant::now() < deadline,
                "private test client did not connect"
            );
        }
    }

    #[test]
    fn names_preserve_unicode_and_metacharacters_without_aliases() {
        assert_ne!(
            endpoint("pane-é").unwrap(),
            endpoint("pane-e\u{301}").unwrap()
        );
        assert_ne!(endpoint("Pane").unwrap(), endpoint("pane").unwrap());
        assert!(endpoint("space 雪 & | ^ % $ ; '").is_ok());
        assert!(endpoint("").is_err());
        assert!(endpoint("a\0b").is_err());
        assert!(validate_endpoint(Path::new(r"\\remote\pipe\tmuxx-test")).is_err());
        assert!(validate_endpoint(Path::new(r"\\.\pipe\unrelated")).is_err());
    }

    #[test]
    fn absent_instance_does_not_launch_an_owner() {
        let path = private_endpoint("absent");
        assert!(connect(&path).is_err());
        assert!(!discover().unwrap().contains(&path));
    }

    #[test]
    fn busy_admission_waits_for_the_same_listener_and_times_out_without_startup() {
        let path = private_endpoint("busy-admission");
        let mut listener = Listener::bind(&path).unwrap();
        let first = connect(&path).unwrap();
        let started = Instant::now();
        let error = connect_timeout(&path, Duration::from_millis(50))
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(discover().unwrap().contains(&path));

        let target = path.clone();
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let second = std::thread::spawn(move || sent.send(connect(&target)).unwrap());
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        let accepted_first = accept(&mut listener);
        let mut accepted_second = accept(&mut listener);
        let mut second_client = received
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        second_client.write_all(b"same-owner").unwrap();
        accepted_second
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut data = [0; 10];
        accepted_second.read_exact(&mut data).unwrap();
        assert_eq!(&data, b"same-owner");
        second.join().unwrap();
        drop((
            first,
            accepted_first,
            second_client,
            accepted_second,
            listener,
        ));
        assert!(connect(&path).is_err());
    }

    #[test]
    fn rapid_concurrent_admissions_preserve_each_binary_stream() {
        let path = private_endpoint("admission-burst");
        let mut listener = Listener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..32 {
                let mut pipe = accept(&mut listener);
                pipe.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut byte = [0];
                pipe.read_exact(&mut byte).unwrap();
                pipe.verify_client().unwrap();
                assert!(seen.insert(byte[0]));
                pipe.write_all(&byte).unwrap();
            }
            assert_eq!(seen, (0..32).collect());
        });
        std::thread::scope(|scope| {
            for byte in 0..32u8 {
                let path = &path;
                scope.spawn(move || {
                    let mut client = connect(path).unwrap();
                    client
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    client.write_all(&[byte]).unwrap();
                    let mut reply = [0];
                    client.read_exact(&mut reply).unwrap();
                    assert_eq!(reply, [byte]);
                });
            }
        });
        server.join().unwrap();
        assert!(connect(&path).is_err());
    }

    #[test]
    fn pipe_fragments_are_binary_exact_and_tokens_match() {
        let path = private_endpoint("bytes");
        let mut listener = Listener::bind(&path).unwrap();
        assert!(
            Listener::bind(&path).is_err(),
            "second owner must not replace the first"
        );
        assert!(discover().unwrap().contains(&path));
        let server = std::thread::spawn(move || {
            let mut pipe = accept(&mut listener);
            pipe.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut bytes = [0u8; 256];
            pipe.read_exact(&mut bytes[..1]).unwrap();
            pipe.verify_client().unwrap();
            pipe.read_exact(&mut bytes[1..]).unwrap();
            assert_eq!(bytes, std::array::from_fn(|i| i as u8));
            for fragment in bytes.chunks(3) {
                pipe.write_all(fragment).unwrap();
            }
        });
        let mut client = connect(&path).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        for byte in 0..=255u8 {
            client.write_all(&[byte]).unwrap();
        }
        let mut received = [0u8; 256];
        client.read_exact(&mut received).unwrap();
        assert_eq!(received, std::array::from_fn(|i| i as u8));
        server.join().unwrap();
        drop(client);
        assert!(connect(&path).is_err());
    }

    #[test]
    fn cancelling_idle_reads_and_slow_writes_is_bounded() {
        let path = private_endpoint("cancel");
        let mut listener = Listener::bind(&path).unwrap();
        let client = connect(&path).unwrap();
        let mut server = accept(&mut listener);
        server
            .set_read_timeout(Some(Duration::from_millis(25)))
            .unwrap();
        assert_eq!(
            server.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        server
            .set_write_timeout(Some(Duration::from_millis(25)))
            .unwrap();
        assert_eq!(
            server.write_all(&vec![0; 1024 * 1024]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        server.set_read_timeout(None).unwrap();
        let cancel = server.clone();
        let reader = std::thread::spawn(move || server.read(&mut [0]));
        cancel.shutdown(Shutdown::Both).unwrap();
        assert!(reader.join().unwrap().is_err());
        drop(client);
    }
}
