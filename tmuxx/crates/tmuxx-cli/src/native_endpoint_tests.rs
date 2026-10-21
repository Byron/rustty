use super::*;
#[cfg(windows)]
use std::path::Path;

#[test]
fn portable_native_names_reject_paths_controls_and_overlong_names() {
    for name in [
        "", ".", "..", "../work", "/work", "a/b", r"a\b", "a\0b", "a\nb", "a\u{7f}b",
    ] {
        let error = named_native_endpoint_in(name, None, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name:?}");
    }
    for name in ["x".repeat(65), "雪".repeat(22)] {
        assert_eq!(
            named_native_endpoint_in(&name, None, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput,
        );
    }
}

#[cfg(windows)]
#[test]
fn portable_native_names_use_existing_windows_pipes() {
    for name in [
        "default",
        "work",
        "work space 雪",
        "pane.native",
        "a:b",
        "a&b",
        "Pane",
        "pane",
    ] {
        let endpoint = named_native_endpoint_in(name, None, None).unwrap();
        assert_eq!(endpoint, tmuxx_runtime::windows::endpoint(name).unwrap());
        assert_eq!(
            native_endpoint_name_in(&endpoint, None, None).as_deref(),
            Some(name)
        );
    }
    // Existing pipe addresses with non-portable names remain explicit addresses.
    let legacy = tmuxx_runtime::windows::endpoint("a/b").unwrap();
    assert_eq!(native_endpoint_name_in(&legacy, None, None), None);
    assert_eq!(
        native_endpoint_name_in(Path::new(r"\\.\pipe\unrelated"), None, None),
        None
    );
}

#[cfg(windows)]
#[test]
fn native_default_override_never_redirects_explicit_windows_names() {
    let custom = tmuxx_runtime::windows::endpoint("custom").unwrap();
    let overridden = Some(custom.as_os_str().to_owned());
    assert_eq!(
        named_native_endpoint_in("default", overridden.clone(), None).unwrap(),
        custom
    );
    assert_eq!(
        native_endpoint_name_in(&custom, overridden.clone(), None).as_deref(),
        Some("default")
    );
    let work = named_native_endpoint_in("work", overridden.clone(), None).unwrap();
    assert_eq!(work, tmuxx_runtime::windows::endpoint("work").unwrap());
    assert_eq!(
        native_endpoint_name_in(&work, overridden.clone(), None).as_deref(),
        Some("work")
    );
    let former_default = tmuxx_runtime::windows::endpoint("default").unwrap();
    assert_eq!(
        native_endpoint_name_in(&former_default, overridden, None),
        None
    );
}

#[cfg(unix)]
struct Namespace(PathBuf);

#[cfg(unix)]
impl Namespace {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tmuxx-native-names-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn temporary(&self) -> Option<OsString> {
        Some(self.0.as_os_str().to_owned())
    }
}

#[cfg(unix)]
impl Drop for Namespace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn portable_native_names_use_the_unix_label_namespace() {
    let namespace = Namespace::new();
    for name in [
        "default",
        "work",
        "work space 雪",
        "pane.native",
        "a:b",
        "a&b",
        "Pane",
        "pane",
    ] {
        let endpoint = named_native_endpoint_in(name, None, namespace.temporary()).unwrap();
        let socket = socket_path(None, Some(name), None, namespace.temporary()).unwrap();
        assert_eq!(endpoint, native_path(&socket));
        assert_eq!(
            native_endpoint_name_in(&endpoint, None, namespace.temporary()).as_deref(),
            Some(name)
        );
    }
    let outside = namespace.0.join("work.native");
    assert_eq!(
        native_endpoint_name_in(&outside, None, namespace.temporary()),
        None
    );
    let plain_socket = socket_path(None, Some("work"), None, namespace.temporary()).unwrap();
    assert_eq!(
        native_endpoint_name_in(&plain_socket, None, namespace.temporary()),
        None
    );
}

#[cfg(unix)]
#[test]
fn native_default_override_never_redirects_explicit_unix_names() {
    let namespace = Namespace::new();
    let custom = namespace.0.join("custom.sock");
    let overridden = Some(custom.as_os_str().to_owned());
    assert_eq!(
        named_native_endpoint_in("default", overridden.clone(), namespace.temporary()).unwrap(),
        custom
    );
    assert_eq!(
        native_endpoint_name_in(&custom, overridden.clone(), namespace.temporary()).as_deref(),
        Some("default")
    );
    let work = named_native_endpoint_in("work", overridden.clone(), namespace.temporary()).unwrap();
    assert_eq!(
        work,
        native_path(&socket_path(None, Some("work"), None, namespace.temporary()).unwrap())
    );
    assert_eq!(
        native_endpoint_name_in(&work, overridden.clone(), namespace.temporary()).as_deref(),
        Some("work")
    );
    let former_default = named_native_endpoint_in("default", None, namespace.temporary()).unwrap();
    assert_eq!(
        native_endpoint_name_in(&former_default, overridden, namespace.temporary()),
        None
    );
}
