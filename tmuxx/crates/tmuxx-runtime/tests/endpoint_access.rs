#![cfg(any(unix, windows))]

#[cfg(windows)]
#[path = "endpoint_access/windows.rs"]
mod windows;

#[cfg(unix)]
mod unix {

    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = PathBuf::from("/tmp").join(format!(
                "tmuxx-access-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn assert_private_socket(path: &Path) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    }

    pub(super) fn native_and_ipc_endpoints_are_owner_private_and_lock_symlinks_are_rejected() {
        let directory = Directory::new();
        let native = directory.path("native");
        let ipc = directory.path("ipc");
        let mut options = tmuxx_runtime::Options::new(&native);
        options.tmux_endpoint = Some(ipc.clone());
        let host = tmuxx_runtime::start(options).unwrap();
        for path in [&native, &ipc] {
            assert_private_socket(path);
            let lock = std::fs::metadata(path.with_extension("lock")).unwrap();
            assert!(lock.is_file());
            assert_eq!(lock.permissions().mode() & 0o077, 0);
            assert_eq!(lock.uid(), unsafe { libc::geteuid() });
        }
        let mut peer = UnixStream::connect(&native).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tmuxx_runtime::native_handshake(&mut peer).unwrap();
        drop(peer);
        host.shutdown().unwrap();
        assert!(!native.exists());
        assert!(!ipc.exists());

        let rejected = directory.path("rejected");
        let target = directory.path("unrelated");
        std::fs::write(&target, b"unchanged").unwrap();
        symlink(&target, rejected.with_extension("lock")).unwrap();
        assert!(tmuxx_runtime::start(tmuxx_runtime::Options::new(&rejected)).is_err());
        assert!(!rejected.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        assert!(
            std::fs::symlink_metadata(rejected.with_extension("lock"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn native_and_ipc_endpoints_are_owner_private_and_lock_symlinks_are_rejected() {
    #[cfg(unix)]
    unix::native_and_ipc_endpoints_are_owner_private_and_lock_symlinks_are_rejected();
    #[cfg(windows)]
    windows::owner_private_endpoints();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires passwordless sudo to run an isolated nobody socket client"]
fn linux_native_and_ipc_reject_other_uid_with_and_without_mode_barrier() {
    let directory = Directory::new();
    std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    let native = directory.path("native");
    let ipc = directory.path("ipc");
    let mut options = tmuxx_runtime::Options::new(&native);
    options.tmux_endpoint = Some(ipc.clone());
    let host = tmuxx_runtime::start(options).unwrap();
    let script = r#"
import errno, os, socket, sys
assert os.geteuid() != int(sys.argv[1]), 'test requires a distinct real OS user'
for path in sys.argv[3:]:
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(3)
        try:
            client.connect(path)
        except PermissionError:
            assert sys.argv[2] == 'private'
        else:
            assert sys.argv[2] == 'peer', 'private socket admitted a different user'
            try:
                data = client.recv(1)
            except ConnectionResetError:
                data = b''
            assert data == b'', 'unauthorized peer received protocol output'
print('both endpoints rejected the distinct uid')
"#;
    for mode in ["private", "peer"] {
        if mode == "peer" {
            // Bypass only these fixture modes to exercise SO_PEERCRED itself.
            for path in [&native, &ipc] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666)).unwrap();
            }
        } else {
            assert_private_socket(&native);
            assert_private_socket(&ipc);
        }
        let result = std::process::Command::new("sudo")
            .args(["-n", "-u", "nobody", "--", "/usr/bin/python3", "-c", script])
            .arg(unsafe { libc::geteuid() }.to_string())
            .arg(mode)
            .args([&native, &ipc])
            .output()
            .expect("sudo and the nobody account are required for this exact access test");
        assert!(result.status.success(), "{mode}: {result:?}");
        assert_eq!(result.stdout, b"both endpoints rejected the distinct uid\n");
        let mut same_user = UnixStream::connect(&native).unwrap();
        same_user
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        tmuxx_runtime::native_handshake(&mut same_user).unwrap();
    }
    host.shutdown().unwrap();
}
