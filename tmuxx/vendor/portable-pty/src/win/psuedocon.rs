use super::WinChild;
use crate::cmdbuilder::CommandBuilder;
use crate::win::procthreadattr::ProcThreadAttributeList;
use anyhow::{bail, ensure, Error};
use filedescriptor::{FileDescriptor, OwnedHandle};
use shared_library::shared_library;
use std::ffi::OsString;
use std::io::Error as IoError;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::{mem, ptr};
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::{HRESULT, S_OK};
use winapi::um::errhandlingapi::{GetThreadErrorMode, SetThreadErrorMode};
use winapi::um::handleapi::*;
use winapi::um::jobapi2::{
    CreateJobObjectW, QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use winapi::um::libloaderapi::{
    FreeLibrary, GetModuleFileNameW, GetModuleHandleExW, LoadLibraryExW,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use winapi::um::processthreadsapi::*;
use winapi::um::winbase::{
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, SEM_FAILCRITICALERRORS,
    SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};
use winapi::um::wincon::COORD;
use winapi::um::winnt::{
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation, HANDLE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

pub type HPCON = HANDLE;

pub const PSUEDOCONSOLE_INHERIT_CURSOR: DWORD = 0x1;
pub const PSEUDOCONSOLE_RESIZE_QUIRK: DWORD = 0x2;
pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: DWORD = 0x4;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_PASSTHROUGH_MODE: DWORD = 0x8;

// Invalid executables must report a native startup error instead of opening a
// modal system dialog. Alter only the spawning thread, preserving the GUI's
// process-wide policy and the caller's thread policy on every return path.
struct SpawnErrorMode(DWORD);

impl SpawnErrorMode {
    fn enter() -> std::io::Result<Self> {
        let previous = unsafe { GetThreadErrorMode() };
        let mode =
            previous | SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX;
        if unsafe { SetThreadErrorMode(mode, ptr::null_mut()) } == 0 {
            return Err(IoError::last_os_error());
        }
        Ok(Self(previous))
    }
}

impl Drop for SpawnErrorMode {
    fn drop(&mut self) {
        unsafe { SetThreadErrorMode(self.0, ptr::null_mut()) };
    }
}

shared_library!(ConPtyFuncs,
    pub fn CreatePseudoConsole(
        size: COORD,
        hInput: HANDLE,
        hOutput: HANDLE,
        flags: DWORD,
        hpc: *mut HPCON
    ) -> HRESULT,
    pub fn ResizePseudoConsole(hpc: HPCON, size: COORD) -> HRESULT,
    pub fn ClosePseudoConsole(hpc: HPCON),
);

static CONPTY: OnceLock<(PathBuf, ConPtyFuncs)> = OnceLock::new();

pub(super) fn initialize_bundled(path: &Path) -> anyhow::Result<()> {
    let path = path.canonicalize()?;
    if let Some((selected, _)) = CONPTY.get() {
        ensure!(
            *selected == path,
            "ConPTY was already initialized from another runtime"
        );
        return Ok(());
    }
    use std::os::windows::ffi::OsStrExt;
    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Resolve dependencies only from the verified bundle and System32. The
    // shared_library lookup below reuses this loaded module and retains it.
    let preloaded =
        unsafe { LoadLibraryExW(wide_path.as_ptr(), ptr::null_mut(), 0x00000100 | 0x00000800) };
    ensure!(
        !preloaded.is_null(),
        "loading bundled ConPTY securely: {}",
        IoError::last_os_error()
    );
    let loaded_api = ConPtyFuncs::open(&path);
    unsafe {
        FreeLibrary(preloaded);
    }
    let api = loaded_api.map_err(|error| anyhow::anyhow!("loading bundled ConPTY: {:?}", error))?;
    // An absolute LoadLibrary path is not sufficient when a module with the
    // same basename was loaded earlier. Verify the module providing the API.
    let mut module = ptr::null_mut();
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            api.CreatePseudoConsole as *const u16,
            &mut module,
        )
    };
    ensure!(
        found != 0,
        "cannot identify loaded ConPTY: {}",
        IoError::last_os_error()
    );
    let mut name = vec![0u16; 32768];
    let length = unsafe { GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) };
    ensure!(
        length != 0 && (length as usize) < name.len(),
        "cannot read loaded ConPTY path"
    );
    let loaded = PathBuf::from(OsString::from_wide(&name[..length as usize])).canonicalize()?;
    ensure!(
        loaded == path,
        "loaded ConPTY differs from the verified bundled runtime"
    );
    match CONPTY.set((path.clone(), api)) {
        Ok(()) => Ok(()),
        Err(_) => {
            ensure!(
                CONPTY.get().map(|(selected, _)| selected) == Some(&path),
                "ConPTY initialization raced with another runtime"
            );
            Ok(())
        }
    }
}

pub struct PsuedoCon {
    con: HPCON,
    api: &'static ConPtyFuncs,
    // Kept by the master/slave pair, even after the initial shell exits.
    // Closing the owner process closes this non-inherited handle atomically.
    job: OwnedHandle,
    closed: AtomicBool,
    api_lock: Mutex<()>,
}

unsafe impl Send for PsuedoCon {}
unsafe impl Sync for PsuedoCon {}

impl Drop for PsuedoCon {
    fn drop(&mut self) {
        // ClosePseudoConsole may wait for peers. End every pane-owned process
        // first so a background descendant cannot hold its streams open.
        unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, 1) };
        self.close();
    }
}

impl PsuedoCon {
    pub fn new(size: COORD, input: FileDescriptor, output: FileDescriptor) -> Result<Self, Error> {
        let api = &CONPTY
            .get()
            .ok_or_else(|| {
                anyhow::anyhow!("bundled ConPTY must be initialized before opening a PTY")
            })?
            .1;
        let handle = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null()) };
        ensure!(
            !handle.is_null(),
            "creating pane job failed: {}",
            IoError::last_os_error()
        );
        let job = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        ensure!(
            unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    mem::size_of_val(&limits) as u32,
                )
            } != 0,
            "setting pane job limits failed: {}",
            IoError::last_os_error()
        );
        let mut con: HPCON = INVALID_HANDLE_VALUE;
        let result = unsafe {
            (api.CreatePseudoConsole)(
                size,
                input.as_raw_handle() as _,
                output.as_raw_handle() as _,
                PSUEDOCONSOLE_INHERIT_CURSOR
                    | PSEUDOCONSOLE_RESIZE_QUIRK
                    | PSEUDOCONSOLE_WIN32_INPUT_MODE,
                &mut con,
            )
        };
        ensure!(
            result == S_OK,
            "failed to create psuedo console: HRESULT {}",
            result
        );
        Ok(Self {
            con,
            api,
            job,
            closed: AtomicBool::new(false),
            api_lock: Mutex::new(()),
        })
    }

    fn close(&self) {
        let _guard = self.api_lock.lock().unwrap();
        if !self.closed.swap(true, Ordering::AcqRel) {
            unsafe {
                (self.api.ClosePseudoConsole)(self.con);
            }
        }
    }

    pub(super) fn close_if_empty(&self) -> std::io::Result<bool> {
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { mem::zeroed() };
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle() as HANDLE,
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                mem::size_of_val(&accounting) as u32,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(IoError::last_os_error());
        }
        if accounting.ActiveProcesses == 0 {
            self.close();
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn terminate(&self) -> std::io::Result<()> {
        if unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, 1) } == 0 {
            return Err(IoError::last_os_error());
        }
        self.close();
        Ok(())
    }

    pub fn resize(&self, size: COORD) -> Result<(), Error> {
        let _guard = self.api_lock.lock().unwrap();
        ensure!(
            !self.closed.load(Ordering::Acquire),
            "pseudo console has closed"
        );
        let result = unsafe { (self.api.ResizePseudoConsole)(self.con, size) };
        ensure!(
            result == S_OK,
            "failed to resize console to {}x{}: HRESULT: {}",
            size.X,
            size.Y,
            result
        );
        Ok(())
    }

    pub fn spawn_command(self: &Arc<Self>, cmd: CommandBuilder) -> anyhow::Result<WinChild> {
        let _guard = self.api_lock.lock().unwrap();
        ensure!(
            !self.closed.load(Ordering::Acquire),
            "pseudo console has closed"
        );
        let mut si: STARTUPINFOEXW = unsafe { mem::zeroed() };
        si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        // Explicitly set the stdio handles as invalid handles otherwise
        // we can end up with a weird state where the spawned process can
        // inherit the explicitly redirected output handles from its parent.
        // For example, when daemonizing wezterm-mux-server, the stdio handles
        // are redirected to a log file and the spawned process would end up
        // writing its output there instead of to the pty we just created.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

        let mut attrs = ProcThreadAttributeList::with_capacity(2)?;
        attrs.set_pty(self.con)?;
        let mut job = self.job.as_raw_handle() as HANDLE;
        attrs.set_job(&mut job)?;
        si.lpAttributeList = attrs.as_mut_ptr();

        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let (mut exe, mut cmdline) = cmd.cmdline()?;
        let cmd_os = OsString::from_wide(&cmdline);

        let cwd = cmd.current_directory();

        let mut environment = cmd.environment_block()?;
        let error = {
            let _error_mode = SpawnErrorMode::enter()?;
            let res = unsafe {
                CreateProcessW(
                    exe.as_mut_slice().as_mut_ptr(),
                    cmdline.as_mut_slice().as_mut_ptr(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                    environment.as_mut_slice().as_mut_ptr() as *mut _,
                    cwd.as_ref()
                        .map(|c| c.as_slice().as_ptr())
                        .unwrap_or(ptr::null()),
                    &mut si.StartupInfo,
                    &mut pi,
                )
            };
            // Restoration can change GetLastError; retain the launch error first.
            (res == 0).then(IoError::last_os_error)
        };
        if let Some(err) = error {
            let msg = format!(
                "CreateProcessW `{:?}` in cwd `{:?}` failed: {}",
                cmd_os,
                cwd.as_ref().map(|c| OsString::from_wide(c)),
                err
            );
            log::error!("{}", msg);
            bail!("{}", msg);
        }

        // Make sure we close out the thread handle so we don't leak it;
        // we do this simply by making it owned
        let _main_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) };

        Ok(WinChild {
            proc: Mutex::new(proc),
            con: self.clone(),
            future_waker: None,
        })
    }
}
