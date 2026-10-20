use super::*;

#[cfg(target_os = "macos")]
fn foreground(fd: libc::c_int) -> (Option<String>, Option<String>) {
    let pid = unsafe { libc::tcgetpgrp(fd) };
    if pid <= 0 {
        return (None, None);
    }
    fn info<T>(pid: libc::pid_t, flavor: libc::c_int) -> Option<T> {
        let mut data = std::mem::MaybeUninit::<T>::zeroed();
        let size = std::mem::size_of::<T>();
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                flavor,
                0,
                data.as_mut_ptr().cast(),
                size as libc::c_int,
            )
        };
        (read as usize == size).then(|| unsafe { data.assume_init() })
    }
    fn string(bytes: &[libc::c_char]) -> Option<String> {
        let bytes = bytes
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| *b as u8)
            .collect();
        String::from_utf8(bytes).ok().filter(|s| !s.is_empty())
    }
    let name = info::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO)
        .and_then(|data| string(&data.pbsi_comm));
    let cwd = info::<libc::proc_vnodepathinfo>(pid, libc::PROC_PIDVNODEPATHINFO)
        .and_then(|data| string(data.pvi_cdir.vip_path.as_flattened()));
    (name, cwd)
}

/// Windows has no POSIX foreground process group for a ConPTY. Inspect the
/// original pane process through its retained handle, never a recycled PID.
#[cfg(windows)]
fn windows_process(handle: &std::os::windows::io::OwnedHandle) -> (Option<String>, Option<String>) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::{QueryFullProcessImageNameW, WaitForSingleObject};
    if unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } != WAIT_TIMEOUT {
        return (None, None);
    }
    let mut image = vec![0u16; 32768];
    let mut length = image.len() as u32;
    let name = if unsafe {
        QueryFullProcessImageNameW(handle.as_raw_handle(), 0, image.as_mut_ptr(), &mut length)
    } != 0
    {
        String::from_utf16(&image[..length as usize])
            .ok()
            .and_then(|path| Path::new(&path).file_name()?.to_str().map(str::to_owned))
    } else {
        None
    };
    (name, windows_current_directory(handle))
}

#[cfg(windows)]
fn windows_current_directory(handle: &std::os::windows::io::OwnedHandle) -> Option<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    // ProcessBasicInformation and ProcessWow64Information expose the PEB.
    // The current-directory UNICODE_STRING occupies the stable NT process-
    // parameters prefix (x64 and WOW64). Every remote read is bounded and
    // checked; compare the header twice to reject a concurrent directory change.
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process: *mut std::ffi::c_void,
            class: u32,
            information: *mut std::ffi::c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    let process = handle.as_raw_handle();
    let read = |address: usize, bytes: &mut [u8]| -> Option<()> {
        if address == 0 {
            return None;
        }
        address.checked_add(bytes.len())?;
        let mut count = 0;
        (unsafe {
            ReadProcessMemory(
                process,
                address as *const _,
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                &mut count,
            )
        } != 0
            && count == bytes.len())
        .then_some(())
    };
    let mut info = [0usize; 6];
    if unsafe {
        NtQueryInformationProcess(
            process,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(&info) as u32,
            std::ptr::null_mut(),
        )
    } < 0
    {
        return None;
    }
    let mut wow64 = 0usize;
    if unsafe {
        NtQueryInformationProcess(
            process,
            26,
            (&mut wow64 as *mut usize).cast(),
            std::mem::size_of::<usize>() as u32,
            std::ptr::null_mut(),
        )
    } < 0
    {
        return None;
    }
    let narrow = wow64 != 0 || cfg!(target_pointer_width = "32");
    let peb = if wow64 != 0 { wow64 } else { info[1] };
    let mut pointer = [0u8; 8];
    let width = if narrow { 4 } else { 8 };
    read(
        peb.checked_add(if narrow { 0x10 } else { 0x20 })?,
        &mut pointer[..width],
    )?;
    let parameters = u64::from_le_bytes(pointer) as usize;
    let address = parameters.checked_add(if narrow { 0x24 } else { 0x38 })?;
    for _ in 0..3 {
        let mut header = [0u8; 16];
        let header_length = if narrow { 8 } else { 16 };
        read(address, &mut header[..header_length])?;
        let length = u16::from_le_bytes([header[0], header[1]]) as usize;
        let maximum = u16::from_le_bytes([header[2], header[3]]) as usize;
        if length == 0 || !length.is_multiple_of(2) || length > maximum || length > 65534 {
            return None;
        }
        let buffer = if narrow {
            u32::from_le_bytes(header[4..8].try_into().ok()?) as usize
        } else {
            u64::from_le_bytes(header[8..16].try_into().ok()?) as usize
        };
        let mut bytes = vec![0u8; length];
        read(buffer, &mut bytes)?;
        let mut after = [0u8; 16];
        read(address, &mut after[..header_length])?;
        if header != after {
            continue;
        }
        let units = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&units).ok();
    }
    None
}
#[cfg(target_os = "linux")]
fn foreground(fd: libc::c_int) -> (Option<String>, Option<String>) {
    let pid = unsafe { libc::tcgetpgrp(fd) };
    if pid <= 0 {
        return (None, None);
    }
    let name = std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            file.take(65536).read_to_end(&mut bytes).ok()?;
            String::from_utf8(bytes.split(|b| *b == 0).next()?.to_vec()).ok()
        });
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok());
    (name, cwd)
}

impl Host {
    pub(super) fn refresh_process_metadata(&mut self) {
        #[cfg(windows)]
        for (id, process) in &self.processes {
            if let Some(pane) = self.engine.panes.get_mut(id) {
                let (name, cwd) = process
                    .handle
                    .as_ref()
                    .map(windows_process)
                    .unwrap_or_default();
                pane.current_command = name;
                pane.current_path = cwd;
            }
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        for (id, process) in &self.processes {
            if let Some(pane) = self.engine.panes.get_mut(id) {
                let (name, cwd) = process
                    .master
                    .as_raw_fd()
                    .map(foreground)
                    .unwrap_or_default();
                pane.current_command = name;
                pane.current_path = cwd;
            }
        }
    }
    pub(super) fn check_window_names(&mut self) {
        if Instant::now() < self.next_name_check {
            return;
        }
        self.next_name_check = Instant::now() + Duration::from_millis(500);
        self.refresh_process_metadata();
        let before = self.hook_snapshot();
        let out = self.engine.rename_automatic_windows();
        if out.changed {
            self.engine.sequence += 1;
            for data in out.notifications {
                self.broadcast_notification(data);
            }
            self.broadcast(ServerMessage::Changed {
                sequence: self.engine.sequence,
            });
            if let Some(before) = before {
                self.graph_hooks(before, false);
            }
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::{Authorization::*, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "tmuxx-cwd-{}-{} 雪 é &",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            assert!(path.is_absolute());
            let sddl: Vec<u16> = format!("D:P(A;;FA;;;{})", crate::windows::account_sid().unwrap())
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = std::ptr::null_mut();
            assert_ne!(
                unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        SDDL_REVISION_1,
                        &mut descriptor,
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let created = unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) };
            unsafe {
                LocalFree(descriptor);
            }
            assert_ne!(created, 0, "{}", io::Error::last_os_error());
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn root_handle_metadata_tracks_native_cd_and_never_reuses_an_exited_pid() {
        let directory = Directory::new();
        let environment = std::env::vars().collect();
        let shell = crate::defaults::system_shell();
        let source = format!(
            "cd /d \"{}\" & echo READY & set /p TMUXX_WAIT=",
            directory.0.display()
        );
        let mut child = crate::windows_job::spawn_shell(
            &shell,
            source.as_bytes(),
            &std::env::current_dir().unwrap(),
            &environment,
            true,
            true,
            true,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (name, cwd) = windows_process(child.handle());
            if cwd.as_deref().is_some_and(|cwd| {
                Path::new(cwd).canonicalize().ok() == directory.0.canonicalize().ok()
            }) {
                assert!(name.unwrap().eq_ignore_ascii_case("cmd.exe"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "root process did not enter Unicode fixture directory: {cwd:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let retained = child.handle().try_clone().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let mut unrelated = crate::windows_job::spawn_shell(
            &shell,
            b"set /p TMUXX_WAIT=",
            &std::env::current_dir().unwrap(),
            &environment,
            true,
            false,
            false,
        )
        .unwrap();
        assert_eq!(windows_process(&retained), (None, None));
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }
}
