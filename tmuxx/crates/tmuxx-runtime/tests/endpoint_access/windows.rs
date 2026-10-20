use std::{
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
};

fn assert_private_pipe(path: &Path) {
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        raw,
        INVALID_HANDLE_VALUE,
        "{}",
        std::io::Error::last_os_error()
    );
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut owner = std::ptr::null_mut();
    let mut acl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            GetSecurityInfo(
                handle.as_raw_handle(),
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut acl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        },
        0
    );
    struct Descriptor(*mut std::ffi::c_void);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    let _descriptor = Descriptor(descriptor);
    assert!(!owner.is_null() && !acl.is_null());
    let mut control = 0;
    let mut revision = 0;
    assert_ne!(
        unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
        0
    );
    assert_ne!(
        control & SE_DACL_PROTECTED,
        0,
        "no inherited account grants"
    );
    assert_eq!(
        unsafe { (*acl).AceCount },
        1,
        "only the owner account may open the pipe"
    );
    let mut ace = std::ptr::null_mut();
    assert_ne!(unsafe { GetAce(acl, 0, &mut ace) }, 0);
    let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    assert_eq!(ace.Header.AceType, 0, "ACCESS_ALLOWED_ACE_TYPE");
    assert_eq!(ace.Header.AceFlags, 0);
    assert_eq!(ace.Mask & FILE_ALL_ACCESS, FILE_ALL_ACCESS);
    let ace_sid = (&ace.SidStart as *const u32).cast_mut().cast();
    assert_ne!(unsafe { EqualSid(owner, ace_sid) }, 0);
    let sid = tmuxx_runtime::windows::account_sid().unwrap();
    let sid_wide: Vec<u16> = sid.encode_utf16().chain(Some(0)).collect();
    let mut expected = std::ptr::null_mut();
    assert_ne!(
        unsafe { ConvertStringSidToSidW(sid_wide.as_ptr(), &mut expected) },
        0
    );
    let _expected = Descriptor(expected);
    assert_ne!(
        unsafe { EqualSid(owner, expected) },
        0,
        "ACL uses account SID across logon sessions"
    );
}

pub(super) fn owner_private_endpoints() {
    use tmuxx_runtime::{
        Options, native_handshake, start,
        windows::{connect, discover, endpoint},
    };
    let instance = format!("access-{}-雪 &", std::process::id());
    let native = endpoint(&instance).unwrap();
    let host = start(Options::new(&native)).unwrap();
    assert_private_pipe(&native);
    assert!(
        start(Options::new(&native)).is_err(),
        "atomic first-instance ownership"
    );
    assert!(discover().unwrap().contains(&native));
    let mut peer = connect(&native).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let first = native_handshake(&mut peer).unwrap();
    drop(peer);
    host.shutdown().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while discover().unwrap().contains(&native) {
        assert!(Instant::now() < deadline, "shutdown retained named pipe");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(connect(&native).is_err());
    let replacement = start(Options::new(&native)).unwrap();
    let mut peer = connect(&native).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let second = native_handshake(&mut peer).unwrap();
    assert!(first.instance_id.is_some());
    assert_ne!(first.instance_id, second.instance_id);
    drop(peer);
    replacement.shutdown().unwrap();

    // Windows has neither filesystem locks nor a Unix descriptor endpoint.
    // Reject both incompatible IPC configuration and filesystem redirection
    // before creating an owner, preserving an unrelated target verbatim.
    let root = std::env::temp_dir().join(format!("tmuxx-access-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _root = Directory(root.clone());
    let target = root.join("unrelated");
    let rejected = root.join("rejected");
    std::fs::write(&target, b"unchanged").unwrap();
    std::os::windows::fs::symlink_file(&target, rejected.with_extension("lock"))
        .expect("Windows symbolic-link privilege is required for endpoint redirection conformance");
    assert!(start(Options::new(&rejected)).is_err());
    assert!(!rejected.exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
    assert!(
        std::fs::symlink_metadata(rejected.with_extension("lock"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let mut incompatible = Options::new(&native);
    incompatible.tmux_endpoint = Some(root.join("ipc"));
    assert_eq!(
        start(incompatible).err().unwrap().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert!(!discover().unwrap().contains(&native));
}
