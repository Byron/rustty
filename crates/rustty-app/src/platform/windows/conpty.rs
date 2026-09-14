//! Load the bundled ConPTY before portable-pty initializes its process-wide API.
use std::{os::windows::ffi::OsStrExt, path::Path, sync::OnceLock};
use windows::{
    Win32::{
        Foundation::FreeLibrary,
        System::LibraryLoader::{
            GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
            LoadLibraryExW,
        },
    },
    core::{PCSTR, PCWSTR},
};

pub fn initialize(resources: &Path) -> Result<(), String> {
    static LOADED: OnceLock<Result<(), String>> = OnceLock::new();
    LOADED.get_or_init(|| load(resources)).clone()
}

fn load(resources: &Path) -> Result<(), String> {
    let directory = resources.join("conpty");
    for name in ["conpty.dll", "OpenConsole.exe"] {
        if !directory.join(name).is_file() {
            return Err(format!(
                "Missing bundled ConPTY runtime: {}. Build or copy the complete Rustty folder, including resources.",
                directory.join(name).display()
            ));
        }
    }
    let library = directory
        .join("conpty.dll")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let wide: Vec<u16> = library.as_os_str().encode_wide().chain(Some(0)).collect();
    // Use an absolute path and restrict dependent DLLs to this directory and
    // System32. Never select a terminal runtime from the shell's working folder.
    let module = unsafe {
        LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }
    .map_err(|e| format!("Could not load {}: {e}", library.display()))?;
    for name in [
        c"CreatePseudoConsole",
        c"ResizePseudoConsole",
        c"ClosePseudoConsole",
    ] {
        if unsafe { GetProcAddress(module, PCSTR(name.as_ptr().cast())) }.is_none() {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err(format!(
                "Bundled ConPTY is missing {}",
                name.to_string_lossy()
            ));
        }
    }
    // portable-pty's later LoadLibrary("conpty.dll") reuses this module. Keep
    // our reference for the process lifetime: PTY worker threads can outlive UI.
    eprintln!("Rustty PTY: {}", library.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_bundles_fail_before_loading_a_system_fallback() {
        let directory =
            std::env::temp_dir().join(format!("rustty-conpty-missing-{}", std::process::id()));
        let error = load(&directory).unwrap_err();
        assert!(error.contains("Missing bundled ConPTY runtime"));
        assert!(error.contains("conpty.dll"));
    }
}
