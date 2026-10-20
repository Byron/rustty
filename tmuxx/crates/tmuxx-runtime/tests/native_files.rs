#![cfg(any(unix, windows))]
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, symlink};
#[cfg(windows)]
use std::os::windows::fs::symlink_file as symlink;
#[cfg(windows)]
#[path = "support/windows_fixture.rs"]
mod windows_fixture;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tmuxx_protocol::native::{Request, Response, ServerMessage};
use tmuxx_runtime::LocalClient;

fn command(client: &LocalClient, argv: &[&str]) -> Response {
    client
        .send(Request {
            request_id: 1,
            argv: {
                #[cfg(unix)]
                {
                    argv.iter().map(|arg| arg.as_bytes().to_vec()).collect()
                }
                #[cfg(windows)]
                {
                    windows_fixture::argv(argv)
                }
            },
            stdin: None,
            subscribe: false,
            control: false,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(ServerMessage::Reply(reply)) = client.try_recv() {
            return reply;
        }
        assert!(
            Instant::now() < deadline,
            "native owner stopped responding: {argv:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn special_native_files_reject_without_blocking_owner_or_changing_buffers() {
    let root = std::env::temp_dir().join(format!("tmuxx-native-files-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _root = Directory(root.clone());
    #[cfg(unix)]
    let endpoint = root.join("native");
    #[cfg(windows)]
    let endpoint =
        tmuxx_runtime::windows::endpoint(root.file_name().unwrap().to_str().unwrap()).unwrap();
    let host = tmuxx_runtime::start(tmuxx_runtime::Options::new(endpoint)).unwrap();
    let client = host.connect().unwrap();
    assert_eq!(
        command(
            &client,
            &["new-session", "-d", "-s", "files", "sleep", "30"]
        )
        .error,
        None
    );
    let pid = command(&client, &["display-message", "-p", "#{pane_pid}"]).output;
    assert_eq!(
        command(&client, &["set-buffer", "-b", "kept", "original"]).error,
        None
    );
    #[cfg(unix)]
    let fifo = root.join("fifo");
    #[cfg(unix)]
    {
        let encoded = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: the path is NUL-terminated and belongs to this test's private directory.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
    }
    #[cfg(windows)]
    let (fifo, _pipe) = private_pipe();
    let link = root.join("fifo-link");
    symlink(&fifo, &link).unwrap();
    #[cfg(unix)]
    let device = PathBuf::from("/dev/zero");
    #[cfg(windows)]
    let device = PathBuf::from("CONIN$");
    for path in [&fifo, &link, &device] {
        for args in [
            vec!["load-buffer", "-b", "kept", path.to_str().unwrap()],
            vec!["save-buffer", "-b", "kept", path.to_str().unwrap()],
            vec!["source-file", path.to_str().unwrap()],
        ] {
            let response = command(&client, &args);
            assert!(
                response
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("unsupported native file type")
                        || error.contains("not devices or named pipes")),
                "{args:?}: {response:?}"
            );
            assert_eq!(
                command(&client, &["show-buffer", "-b", "kept"]).output,
                b"original"
            );
            assert_eq!(
                command(&client, &["display-message", "-p", "#{pane_pid}"]).output,
                pid
            );
        }
    }
    #[cfg(unix)]
    assert!(std::fs::metadata(&fifo).unwrap().file_type().is_fifo());
    let regular = root.join("regular");
    std::fs::write(&regular, b"regular\0\xff").unwrap();
    let regular_link = root.join("regular-link");
    symlink(&regular, &regular_link).unwrap();
    assert_eq!(
        command(
            &client,
            &["load-buffer", "-b", "kept", regular_link.to_str().unwrap()]
        )
        .error,
        None
    );
    assert_eq!(
        command(&client, &["show-buffer", "-b", "kept"]).output,
        b"regular\0\xff"
    );
    #[cfg(windows)]
    {
        for spelling in [root.join("NUL"), PathBuf::from(r"\\.\NUL")] {
            for verb in ["save-buffer", "load-buffer"] {
                assert_eq!(
                    command(&client, &[verb, "-b", "kept", spelling.to_str().unwrap()]).error,
                    None
                );
            }
            assert_eq!(
                command(&client, &["source-file", spelling.to_str().unwrap()]).error,
                None
            );
        }
        for leaf in ["NUL.txt", "CON", "AUX", "COM1", "LPT1"] {
            let path = root.join(leaf);
            for verb in ["save-buffer", "load-buffer"] {
                assert!(
                    command(&client, &[verb, "-b", "kept", path.to_str().unwrap()])
                        .error
                        .is_some()
                );
            }
        }
        assert_eq!(
            command(&client, &["show-buffer", "-b", "kept"]).output,
            b"regular\0\xff"
        );
        assert_eq!(
            command(&client, &["display-message", "-p", "#{pane_pid}"]).output,
            pid
        );
    }
    let saved = root.join("saved");
    assert_eq!(
        command(
            &client,
            &["save-buffer", "-b", "kept", saved.to_str().unwrap()]
        )
        .error,
        None
    );
    assert_eq!(std::fs::read(&saved).unwrap(), b"regular\0\xff");
    #[cfg(unix)]
    let null = "/dev/null";
    #[cfg(windows)]
    let null = "NUL";
    assert_eq!(
        command(&client, &["save-buffer", "-b", "kept", null]).error,
        None
    );
    assert_eq!(
        command(&client, &["load-buffer", "-b", "kept", null]).error,
        None
    );
    assert_eq!(
        command(&client, &["show-buffer", "-b", "kept"]).output,
        b"regular\0\xff"
    );
    host.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
fn private_pipe() -> (PathBuf, std::os::windows::io::OwnedHandle) {
    use std::os::windows::{
        ffi::OsStrExt,
        io::{FromRawHandle, OwnedHandle},
    };
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX},
        System::Pipes::{CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT},
    };
    let name = PathBuf::from(format!(r"\\.\pipe\tmuxx-file-type-{}", std::process::id()));
    let wide: Vec<u16> = name.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(
        handle,
        INVALID_HANDLE_VALUE,
        "{}",
        std::io::Error::last_os_error()
    );
    (name, unsafe { OwnedHandle::from_raw_handle(handle) })
}
