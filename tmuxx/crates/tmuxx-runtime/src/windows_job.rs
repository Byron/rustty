//! Windows processes enter their owning Job Object in CreateProcessW itself.
//! No thread executes and no orphan window exists before job assignment.
#[cfg(test)]
#[path = "windows_job_conpty_tests.rs"]
mod conpty_tests;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::core::BOOL;

fn checked(value: BOOL) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Suppress startup error dialogs only on this thread; preserve the GUI's
/// process-wide policy and restore every existing caller flag afterward.
struct SpawnErrorMode(u32);
impl SpawnErrorMode {
    fn enter() -> io::Result<Self> {
        use windows_sys::Win32::System::Diagnostics::Debug::*;
        let previous = unsafe { GetThreadErrorMode() };
        checked(unsafe {
            SetThreadErrorMode(
                previous | SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX,
                null_mut(),
            )
        })?;
        Ok(Self(previous))
    }
}
impl Drop for SpawnErrorMode {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetThreadErrorMode(self.0, null_mut());
        }
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value = value.encode_wide().collect::<Vec<_>>();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in Windows process argument",
        ));
    }
    value.push(0);
    Ok(value)
}

#[derive(Debug)]
pub(super) struct Job(OwnedHandle);
impl Job {
    pub(super) fn new() -> io::Result<Arc<Self>> {
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        checked(unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        Ok(Arc::new(job))
    }
    pub(super) fn terminate(&self) -> io::Result<()> {
        checked(unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) })
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

struct Attributes {
    data: Vec<usize>,
}
impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut data = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        checked(unsafe {
            InitializeProcThreadAttributeList(data.as_mut_ptr().cast(), count, 0, &mut bytes)
        })?;
        // Only a successfully initialized list may reach Drop.
        Ok(Self { data })
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.data.as_mut_ptr().cast()
    }
    fn set<T>(&mut self, key: usize, value: &[T]) -> io::Result<()> {
        checked(unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                key,
                value.as_ptr().cast(),
                std::mem::size_of_val(value),
                null_mut(),
                null(),
            )
        })
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.ptr());
        }
    }
}

fn pipe() -> io::Result<(File, File)> {
    let mut read = null_mut();
    let mut write = null_mut();
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    checked(unsafe { CreatePipe(&mut read, &mut write, &security, 0) })?;
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}
fn private(file: &File) -> io::Result<()> {
    checked(unsafe { SetHandleInformation(file.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) })
}

pub(super) struct Reader(File);
impl Read for Reader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                self.0.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                Ok(0)
            } else {
                Err(error)
            };
        }
        if available == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = buffer.len().min(available as usize);
        self.0.read(&mut buffer[..count])
    }
}

pub(super) struct Child {
    process: OwnedHandle,
    id: u32,
    pub(super) stdin: Option<File>,
    pub(super) stdout: Option<Reader>,
    stderr: Option<File>,
    job: Arc<Job>,
}
impl Child {
    pub(super) fn id(&self) -> u32 {
        self.id
    }
    #[cfg(test)]
    pub(super) fn handle(&self) -> &OwnedHandle {
        &self.process
    }
    pub(super) fn ownership(&self) -> Arc<Job> {
        self.job.clone()
    }
    pub(super) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut status = 0;
                checked(unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut status) })?;
                Ok(Some(std::process::ExitStatus::from_raw(status)))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
    pub(super) fn kill(&mut self) -> io::Result<()> {
        self.job.terminate()
    }
    pub(super) fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 5000) } != WAIT_OBJECT_0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "owned process did not exit within five seconds",
            ));
        }
        self.try_wait()?
            .ok_or_else(|| io::Error::other("process wait completed without exit status"))
    }
}

/// CRT argument quoting for executable argv. Shell source is handled separately.
fn quote(value: &OsStr) -> io::Result<Vec<u16>> {
    let value = wide(value)?;
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for &unit in &value[..value.len() - 1] {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            b'\\' as u16,
            slashes * if unit == b'"' as u16 { 2 } else { 1 },
        ));
        if unit == b'"' as u16 {
            result.push(b'\\' as u16);
        }
        result.push(unit);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    Ok(result)
}

/// A binary-stdio subprocess atomically owned by a kill-on-close Job Object.
/// Dropping this handle, or abrupt death of its owner, terminates its descendants.
pub struct OwnedStdioChild {
    child: Child,
    pub stdin: Option<File>,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
}
impl OwnedStdioChild {
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }
    /// Wait at most five seconds; callers cancel a still-running helper first.
    pub fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait()
    }
}

/// Start an absolute executable without a shell or console window. Inherit the
/// owner's native environment and cwd unchanged, pipe binary stdin/stdout and
/// inherit stderr (or NUL when the GUI has no stderr handle).
pub fn spawn_owned_stdio(program: &Path, args: &[OsString]) -> io::Result<OwnedStdioChild> {
    spawn_owned(program, args, ErrorOutput::Inherited)
}

/// Like `spawn_owned_stdio`, with a separate binary stderr pipe for adapters
/// that consume diagnostics themselves. Stdout and stderr are never merged.
pub fn spawn_owned_stdio_piped_stderr(
    program: &Path,
    args: &[OsString],
) -> io::Result<OwnedStdioChild> {
    spawn_owned(program, args, ErrorOutput::Pipe)
}

fn spawn_owned(
    program: &Path,
    args: &[OsString],
    stderr: ErrorOutput,
) -> io::Result<OwnedStdioChild> {
    if !program.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "owned stdio executable path must be absolute",
        ));
    }
    let executable = wide(program.as_os_str())?;
    let mut line = quote(program.as_os_str())?;
    for arg in args {
        line.push(b' ' as u16);
        line.extend(quote(arg)?);
    }
    line.push(0);
    let mut child = spawn_process(&executable, &mut line, None, None, true, true, stderr)?;
    Ok(OwnedStdioChild {
        stdin: child.stdin.take(),
        stdout: child.stdout.take().map(|reader| reader.0),
        stderr: child.stderr.take(),
        child,
    })
}

enum ErrorOutput {
    Null,
    Output,
    Inherited,
    Pipe,
}

/// Construct the command-line tail required by supported Windows shells.
/// cmd's source is deliberately raw after /s /c; CRT quoting would change it.
pub(super) fn shell_tail(shell: &str, command: &[u8]) -> io::Result<OsString> {
    let source = std::str::from_utf8(command).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows shell source must be UTF-8",
        )
    })?;
    if source.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in shell source",
        ));
    }
    let name = Path::new(shell)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    if name.eq_ignore_ascii_case("cmd.exe") || name.eq_ignore_ascii_case("cmd") {
        return Ok(format!("/d /s /c \"{source}\"").into());
    }
    let prefix = if name.eq_ignore_ascii_case("powershell.exe")
        || name.eq_ignore_ascii_case("pwsh.exe")
        || name.eq_ignore_ascii_case("powershell")
        || name.eq_ignore_ascii_case("pwsh")
    {
        "-NoLogo -NoProfile -NonInteractive -Command "
    } else {
        "-c "
    };
    use std::os::windows::ffi::OsStringExt;
    let mut units = prefix.encode_utf16().collect::<Vec<_>>();
    units.extend(quote(OsStr::new(source))?);
    Ok(OsString::from_wide(&units))
}

/// Create a shell with explicit stdio inheritance and atomic job membership.
pub(super) fn spawn_shell(
    shell: &str,
    command: &[u8],
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    input: bool,
    output: bool,
    stderr: bool,
) -> io::Result<Child> {
    let executable = wide(OsStr::new(shell))?;
    let mut line = quote(OsStr::new(shell))?;
    line.push(b' ' as u16);
    line.extend(shell_tail(shell, command)?.encode_wide());
    line.push(0);
    let cwd = wide(cwd.as_os_str())?;
    let mut env = Vec::new();
    let mut sorted = Vec::new();
    for (name, value) in environment {
        // Windows also inherits hidden entries such as =C:, =:: (MSYS),
        // and cmd's =ExitCode. Their leading '=' is part of the name.
        let hidden = name.starts_with('=') && name.len() > 1 && !name[1..].contains('=');
        if name.is_empty()
            || name.contains('\0')
            || (name.contains('=') && !hidden)
            || value.contains('\0')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid Windows environment entry name {name:?}"),
            ));
        }
        sorted.push((name.encode_utf16().collect::<Vec<_>>(), name, value));
    }
    let compare = |left: &[u16], right: &[u16]| {
        let result = unsafe {
            windows_sys::Win32::Globalization::CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            )
        };
        if result == 0 {
            left.cmp(right)
        } else {
            result.cmp(&2)
        }
    };
    sorted.sort_by(|left, right| compare(&left.0, &right.0));
    if sorted
        .windows(2)
        .any(|pair| compare(&pair[0].0, &pair[1].0).is_eq())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "duplicate Windows environment names differ only in ordinal casing",
        ));
    }
    for (_, name, value) in sorted {
        env.extend(wide(OsStr::new(&format!("{name}={value}")))?);
    }
    if env.is_empty() {
        env.push(0);
    }
    env.push(0);
    spawn_process(
        &executable,
        &mut line,
        Some(&cwd),
        Some(&env),
        input,
        output,
        if stderr {
            ErrorOutput::Output
        } else {
            ErrorOutput::Null
        },
    )
}

fn spawn_process(
    executable: &[u16],
    line: &mut [u16],
    cwd: Option<&[u16]>,
    environment: Option<&[u16]>,
    input: bool,
    output: bool,
    stderr: ErrorOutput,
) -> io::Result<Child> {
    let job = Job::new()?;
    let (child_input, parent_input) = pipe()?;
    let (parent_output, child_output) = pipe()?;
    private(&parent_input)?;
    private(&parent_output)?;
    let error_pipe = if matches!(stderr, ErrorOutput::Pipe) {
        let pair = pipe()?;
        private(&pair.0)?;
        Some(pair)
    } else {
        None
    };
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let null_name: Vec<u16> = "NUL\0".encode_utf16().collect();
    let null_handle = unsafe {
        CreateFileW(
            null_name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if null_handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let null_file = unsafe { File::from_raw_handle(null_handle) };
    let inherited_error = if matches!(stderr, ErrorOutput::Inherited) {
        let source = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if source.is_null() || source == INVALID_HANDLE_VALUE {
            None
        } else {
            let mut duplicate = null_mut();
            let current = unsafe { GetCurrentProcess() };
            checked(unsafe {
                DuplicateHandle(
                    current,
                    source,
                    current,
                    &mut duplicate,
                    0,
                    1,
                    DUPLICATE_SAME_ACCESS,
                )
            })?;
            Some(unsafe { File::from_raw_handle(duplicate) })
        }
    } else {
        None
    };
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = child_input.as_raw_handle();
    startup.StartupInfo.hStdOutput = if output {
        child_output.as_raw_handle()
    } else {
        null_handle
    };
    startup.StartupInfo.hStdError = match stderr {
        ErrorOutput::Output => child_output.as_raw_handle(),
        ErrorOutput::Inherited => inherited_error
            .as_ref()
            .map_or(null_handle, AsRawHandle::as_raw_handle),
        ErrorOutput::Null => null_handle,
        ErrorOutput::Pipe => error_pipe
            .as_ref()
            .expect("created stderr pipe")
            .1
            .as_raw_handle(),
    };
    let mut handles = vec![child_input.as_raw_handle(), null_handle];
    if output || matches!(stderr, ErrorOutput::Output) {
        handles.push(child_output.as_raw_handle());
    }
    if let Some(file) = &inherited_error {
        handles.push(file.as_raw_handle());
    }
    if let Some((_, child_error)) = &error_pipe {
        handles.push(child_error.as_raw_handle());
    }
    let jobs = [job.0.as_raw_handle()];
    let mut attributes = Attributes::new(2)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, &handles)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST as usize, &jobs)?;
    startup.lpAttributeList = attributes.ptr();
    let mut information: PROCESS_INFORMATION = unsafe { zeroed() };
    let result = {
        let _error_mode = SpawnErrorMode::enter()?;
        // Retain GetLastError before restoring the caller's thread error mode.
        checked(unsafe {
            CreateProcessW(
                executable.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                environment.map_or(null(), |env| env.as_ptr().cast()),
                cwd.map_or(null(), |cwd| cwd.as_ptr()),
                &startup.StartupInfo,
                &mut information,
            )
        })
    };
    result?;
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let _thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    drop(child_input);
    drop(child_output);
    drop(null_file);
    Ok(Child {
        process,
        id: information.dwProcessId,
        stdin: input.then_some(parent_input),
        stdout: (output || matches!(stderr, ErrorOutput::Output)).then_some(Reader(parent_output)),
        stderr: error_pipe.map(|(parent, _child)| parent),
        job,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn environment() -> BTreeMap<String, String> {
        std::env::vars().collect()
    }
    fn powershell() -> String {
        Path::new(&super::super::defaults::system_shell())
            .parent()
            .unwrap()
            .join("WindowsPowerShell/v1.0/powershell.exe")
            .into_os_string()
            .into_string()
            .unwrap()
    }
    fn collect(child: &mut Child) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = Vec::new();
        let mut reader = child.stdout.take().unwrap();
        let mut buffer = [0; 4096];
        loop {
            assert!(Instant::now() < deadline, "owned fixture timed out");
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    output.extend_from_slice(&buffer[..n]);
                    assert!(output.len() <= 65536);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("fixture output: {error}"),
            }
        }
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "owned fixture did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        output
    }
    fn ps_literal(text: &str) -> String {
        format!("'{}'", text.replace('\'', "''"))
    }

    #[test]
    fn invalid_executable_returns_error_and_restores_thread_error_mode() {
        use windows_sys::Win32::System::Diagnostics::Debug::*;
        const NAME: &str =
            "windows_job::tests::invalid_executable_returns_error_and_restores_thread_error_mode";
        const ROLE: &str = "TMUXX_INVALID_STDIO_FIXTURE";
        let args = std::env::args_os().collect::<Vec<_>>();
        if let Some(position) = args.iter().position(|arg| arg == ROLE) {
            let executable = Path::new(&args[position + 2]).join("invalid.exe");
            let process_mode = unsafe { GetErrorMode() };
            for mode in [0, SEM_NOOPENFILEERRORBOX, SEM_NOGPFAULTERRORBOX] {
                checked(unsafe { SetThreadErrorMode(mode, null_mut()) }).unwrap();
                let error = match spawn_owned_stdio(&executable, &[]) {
                    Err(error) => error,
                    Ok(mut child) => {
                        child.kill().unwrap();
                        child.wait().unwrap();
                        panic!("malformed executable unexpectedly started");
                    }
                };
                assert!(
                    matches!(error.raw_os_error(), Some(193 | 216)),
                    "unexpected native startup error: {error}"
                );
                assert_eq!(unsafe { GetThreadErrorMode() }, mode);
                assert_eq!(unsafe { GetErrorMode() }, process_mode);
                let mut success = spawn_owned_stdio(
                    &std::env::current_exe().unwrap(),
                    &[
                        "--list".into(),
                        "--exact".into(),
                        "no_such_private_fixture_test".into(),
                    ],
                )
                .unwrap();
                assert_eq!(unsafe { GetThreadErrorMode() }, mode);
                assert_eq!(unsafe { GetErrorMode() }, process_mode);
                assert!(success.wait().unwrap().success());
            }
            return;
        }

        struct FixtureDirectory(std::path::PathBuf);
        impl Drop for FixtureDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(self.0.join("invalid.exe"));
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = FixtureDirectory(std::env::temp_dir().join(format!(
            "tmuxx-invalid-stdio-{}-{nonce}",
            std::process::id()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        std::fs::write(
            directory.0.join("invalid.exe"),
            b"private malformed PE fixture",
        )
        .unwrap();
        let mut fixture = spawn_owned_stdio_piped_stderr(
            &std::env::current_exe().unwrap(),
            &[
                "--exact".into(),
                NAME.into(),
                "--nocapture".into(),
                "--skip".into(),
                ROLE.into(),
                "--skip".into(),
                directory.0.as_os_str().to_owned(),
            ],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let completed = fixture.try_wait().unwrap();
        // Keep this boundary alive throughout the assertion and always terminate
        // the exact fixture job before inspecting any captured diagnostics.
        fixture.kill().unwrap();
        fixture.wait().unwrap();
        let mut diagnostics = String::new();
        fixture
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut diagnostics)
            .unwrap();
        assert!(
            completed.is_some_and(|status| status.success()),
            "invalid executable startup did not return successfully within five seconds: {diagnostics}"
        );
    }

    #[test]
    fn owned_stdio_preserves_binary_streams_and_cleans_during_spawn() {
        const MODE: &str = "TMUXX_OWNED_STDIO_FIXTURE";
        const NAME: &str =
            "windows_job::tests::owned_stdio_preserves_binary_streams_and_cleans_during_spawn";
        if std::env::var_os(MODE).is_some() {
            let args: Vec<OsString> = std::env::args_os().collect();
            if let Some(position) = args.iter().position(|value| value == "--skip") {
                use std::os::windows::ffi::OsStringExt;
                let expected = "space & | < > ^ % ! 雪 é \" tail\\"
                    .encode_utf16()
                    .collect::<Vec<_>>();
                assert_eq!(args[position + 1], OsString::from_wide(&expected));
                let mut bytes = Vec::new();
                std::io::stdin().read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, (0u8..=255).collect::<Vec<_>>());
                std::io::Write::write_all(&mut std::io::stdout(), &bytes).unwrap();
                std::io::Write::write_all(&mut std::io::stderr(), b"PRIVATE-STDERR").unwrap();
                return;
            }
            use std::os::windows::ffi::OsStringExt;
            let value = "space & | < > ^ % ! 雪 é \" tail\\"
                .encode_utf16()
                .collect::<Vec<_>>();
            let exe = std::env::current_exe().unwrap();
            let mut child = spawn_owned_stdio_piped_stderr(
                &exe,
                &[
                    "--exact".into(),
                    NAME.into(),
                    "--nocapture".into(),
                    "--skip".into(),
                    OsString::from_wide(&value),
                ],
            )
            .unwrap();
            std::io::Write::write_all(
                child.stdin.as_mut().unwrap(),
                &(0u8..=255).collect::<Vec<_>>(),
            )
            .unwrap();
            child.stdin.take();
            let mut stdout = child.stdout.take().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let output = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = stdout.read_to_end(&mut bytes);
                let _ = tx.send((result, bytes));
            });
            let (tx, rxerr) = std::sync::mpsc::channel();
            let error = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = stderr.read_to_end(&mut bytes);
                let _ = tx.send((result, bytes));
            });
            let received = rx.recv_timeout(Duration::from_secs(10));
            let diagnostics = rxerr.recv_timeout(Duration::from_secs(10));
            let _ = child.kill();
            output.join().unwrap();
            error.join().unwrap();
            let (status, bytes) = received.unwrap();
            status.unwrap();
            assert!(
                bytes
                    .windows(256)
                    .any(|window| window == (0u8..=255).collect::<Vec<_>>()),
                "stdout={bytes:?}; stderr={diagnostics:?}"
            );
            let (status, bytes) = diagnostics.unwrap();
            status.unwrap();
            assert_eq!(bytes, b"PRIVATE-STDERR");
            let mut child = spawn_owned_stdio(
                Path::new(&powershell()),
                &[
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "Start-Sleep 300".into(),
                ],
            )
            .unwrap();
            println!("OWNED:{}", child.id());
            std::io::Write::flush(&mut std::io::stdout()).unwrap();
            // Do not drop the child: job ownership must already exist in the
            // successful CreateProcess, before the caller registers the helper.
            child.stdin.take();
            std::process::exit(0);
        }
        // libtest itself decodes argv as UTF-8 before invoking a test, so use
        // Windows' actual command-line parser for unpaired UTF-16 admission.
        use std::os::windows::ffi::OsStringExt;
        let original = [b'x' as u16, 0xd800, b'"' as u16, b'\\' as u16];
        let mut line = quote(OsStr::new("fixture.exe")).unwrap();
        line.push(b' ' as u16);
        line.extend(quote(&OsString::from_wide(&original)).unwrap());
        line.push(0);
        let mut count = 0;
        let parsed =
            unsafe { windows_sys::Win32::UI::Shell::CommandLineToArgvW(line.as_ptr(), &mut count) };
        assert!(!parsed.is_null());
        assert_eq!(count, 2);
        let value = unsafe { *parsed.add(1) };
        let mut units = Vec::new();
        while unsafe { *value.add(units.len()) } != 0 {
            units.push(unsafe { *value.add(units.len()) });
        }
        unsafe {
            LocalFree(parsed.cast());
        }
        assert_eq!(units, original);
        assert!(spawn_owned_stdio(Path::new("relative.exe"), &[]).is_err());
        assert!(
            spawn_owned_stdio(&std::env::temp_dir().join("tmuxx-no-such-fixture.exe"), &[])
                .is_err()
        );
        let mut env = environment();
        env.insert(MODE.into(), "1".into());
        let exe = std::env::current_exe().unwrap();
        let source = format!(
            "& {} --exact {NAME} --nocapture",
            ps_literal(exe.to_str().unwrap())
        );
        let mut fixture = spawn_shell(
            &powershell(),
            source.as_bytes(),
            &std::env::current_dir().unwrap(),
            &env,
            false,
            true,
            true,
        )
        .unwrap();
        let output = String::from_utf8(collect(&mut fixture)).unwrap();
        assert!(fixture.try_wait().unwrap().unwrap().success(), "{output}");
        let pid = output
            .lines()
            .find_map(|line| line.strip_prefix("OWNED:"))
            .expect(&output)
            .parse::<u32>()
            .unwrap();
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if !handle.is_null() {
            let process = unsafe { OwnedHandle::from_raw_handle(handle) };
            assert_eq!(
                unsafe { WaitForSingleObject(process.as_raw_handle(), 5000) },
                WAIT_OBJECT_0,
                "stdio helper escaped abrupt owner exit"
            );
        }
    }

    #[test]
    fn shell_forms_preserve_unicode_metacharacters_binary_output_and_environment() {
        let cwd = std::env::current_dir().unwrap();
        let mut env = environment();
        env.insert("TMUXX_JOB_VALUE".into(), "space & | < > ^ % ! 雪 é".into());
        let script = "$v=$env:TMUXX_JOB_VALUE; $b=[Text.Encoding]::UTF8.GetBytes($v); [Console]::OpenStandardOutput().Write($b,0,$b.Length); [Console]::OpenStandardOutput().Write([byte[]](0,255,13,10),0,4)";
        let mut child = spawn_shell(
            &powershell(),
            script.as_bytes(),
            &cwd,
            &env,
            false,
            true,
            true,
        )
        .unwrap();
        let output = collect(&mut child);
        assert_eq!(
            output,
            [env["TMUXX_JOB_VALUE"].as_bytes(), &[0, 255, 13, 10]].concat()
        );
        assert!(child.try_wait().unwrap().unwrap().success());
        let mut child = spawn_shell(
            &super::super::defaults::system_shell(),
            b"echo a^&b^|c^<d^>e^^f",
            &cwd,
            &env,
            false,
            true,
            true,
        )
        .unwrap();
        assert_eq!(collect(&mut child), b"a&b|c<d>e^f\r\n");
        assert!(child.try_wait().unwrap().unwrap().success());
        assert!(spawn_shell(&powershell(), &[0xff], &cwd, &env, false, true, true).is_err());
        assert!(spawn_shell(&powershell(), b"echo\0bad", &cwd, &env, false, true, true).is_err());
    }

    #[test]
    fn cancellation_kills_descendants_and_preserves_unrelated_process() {
        let cwd = std::env::current_dir().unwrap();
        let env = environment();
        let script = format!(
            "$p=Start-Process -FilePath {} -ArgumentList '-NoLogo -NoProfile -NonInteractive -Command Start-Sleep 300' -PassThru; [Console]::WriteLine($p.Id); [Console]::Out.Flush(); Start-Sleep 300",
            ps_literal(&powershell())
        );
        let mut owned = spawn_shell(
            &powershell(),
            script.as_bytes(),
            &cwd,
            &env,
            false,
            true,
            true,
        )
        .unwrap();
        let mut unrelated = spawn_shell(
            &powershell(),
            b"Start-Sleep 300",
            &cwd,
            &env,
            false,
            false,
            false,
        )
        .unwrap();
        let mut reader = owned.stdout.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut line = Vec::new();
        while !line.contains(&b'\n') {
            assert!(
                Instant::now() < deadline,
                "descendant did not report identity"
            );
            let mut byte = [0];
            match reader.read(&mut byte) {
                Ok(1) => line.push(byte[0]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                result => panic!("missing descendant identity: {result:?}"),
            }
        }
        let pid: u32 = std::str::from_utf8(&line).unwrap().trim().parse().unwrap();
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null());
        let process = unsafe { OwnedHandle::from_raw_handle(handle) };
        owned.kill().unwrap();
        owned.wait().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 5000) },
            WAIT_OBJECT_0,
            "background descendant survived cancellation"
        );
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "unrelated job was killed"
        );
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }

    #[test]
    fn abrupt_owner_exit_cleans_process_created_before_registration() {
        const MODE: &str = "TMUXX_JOB_IMMEDIATE_EXIT_FIXTURE";
        if std::env::var_os(MODE).is_some() {
            let child = spawn_shell(
                &powershell(),
                b"Start-Sleep 300",
                &std::env::current_dir().unwrap(),
                &environment(),
                false,
                false,
                false,
            )
            .unwrap();
            println!("OWNED:{}", child.id());
            std::io::Write::flush(&mut std::io::stdout()).unwrap();
            // Deliberately bypass all Rust drops immediately after CreateProcess.
            // The OS must close the inner job and kill its initial process.
            std::process::exit(0);
        }
        let mut env = environment();
        env.insert(MODE.into(), "1".into());
        let exe = std::env::current_exe().unwrap();
        let source = format!(
            "& {} --exact windows_job::tests::abrupt_owner_exit_cleans_process_created_before_registration --nocapture",
            ps_literal(exe.to_str().unwrap())
        );
        // The outer job is a test cleanup boundary if the inner assertion fails.
        let mut fixture = spawn_shell(
            &powershell(),
            source.as_bytes(),
            &std::env::current_dir().unwrap(),
            &env,
            false,
            true,
            true,
        )
        .unwrap();
        let output = collect(&mut fixture);
        let output = String::from_utf8(output).unwrap();
        let pid = output
            .lines()
            .find_map(|line| line.strip_prefix("OWNED:"))
            .expect(&output)
            .parse::<u32>()
            .unwrap();
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if !handle.is_null() {
            let process = unsafe { OwnedHandle::from_raw_handle(handle) };
            assert_eq!(
                unsafe { WaitForSingleObject(process.as_raw_handle(), 5000) },
                WAIT_OBJECT_0,
                "process escaped abrupt owner exit"
            );
        }
        assert!(fixture.try_wait().unwrap().unwrap().success());
    }
}
