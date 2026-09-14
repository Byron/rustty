//! Explicit per-user registration for an unpackaged Windows application.
use rustty_app::platform::{APP_USER_MODEL_ID, TOAST_ACTIVATOR_CLSID};
use std::{
    ffi::OsStr,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, PROPERTYKEY},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize, IPersistFile,
                StructuredStorage::{
                    InitPropVariantFromCLSID, PROPVARIANT, PVCHF_DEFAULT, PropVariantChangeType,
                },
            },
            Registry::*,
            Variant::VT_LPWSTR,
        },
        UI::Shell::{
            FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, PropertiesSystem::IPropertyStore,
            SHGetKnownFolderPath, ShellLink,
        },
    },
    core::{GUID, Interface, PCWSTR, w},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Windows' documented AppUserModel property set, defined in propkey.h.
const APP_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};
const ACTIVATOR: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_ID.fmtid,
    pid: 26,
};

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn shortcut_path() -> Result<PathBuf> {
    let directory = unsafe { SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None)? };
    let path = PathBuf::from(unsafe { std::ffi::OsString::from_wide(directory.as_wide()) });
    unsafe {
        CoTaskMemFree(Some(directory.0.cast()));
    }
    Ok(path.join("Rustty.lnk"))
}

fn save_shortcut(path: &Path, executable: &Path) -> Result<()> {
    let exe = wide(executable);
    let filename = wide(path);
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(exe.as_ptr()))?;
        link.SetDescription(w!("Rustty terminal"))?;
        link.SetIconLocation(PCWSTR(exe.as_ptr()), 0)?;
        let store: IPropertyStore = link.cast()?;
        let mut app_id = PROPVARIANT::default();
        PropVariantChangeType(
            &mut app_id,
            &PROPVARIANT::from(APP_USER_MODEL_ID),
            PVCHF_DEFAULT,
            VT_LPWSTR,
        )?;
        store.SetValue(&APP_ID, &app_id)?;
        let guid = GUID::try_from(TOAST_ACTIVATOR_CLSID.trim_matches(['{', '}']))?;
        let activator = InitPropVariantFromCLSID(&guid)?;
        store.SetValue(&ACTIVATOR, &activator)?;
        store.Commit()?;
        link.cast::<IPersistFile>()?
            .Save(PCWSTR(filename.as_ptr()), true)?;
    }
    Ok(())
}

fn set_string(path: &str, name: &str, value: &str) -> Result<()> {
    let path = wide(path);
    let name = wide(name);
    let value = wide(value);
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
    }
    let key = Key(key);
    let bytes: Vec<u8> = value.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    unsafe {
        RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&bytes)).ok()?;
    }
    Ok(())
}

fn registered_executable(path: &str) -> Result<Option<String>> {
    let path = wide(path);
    let mut length = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut length),
        )
    };
    if matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
        return Ok(None);
    }
    status.ok()?;
    let mut buffer = vec![0u16; length as usize / 2];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut length),
        )
        .ok()?;
    }
    Ok(Some(String::from_utf16_lossy(
        &buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())],
    )))
}

/// Registration is an explicit command, never a side effect of a normal launch.
pub fn run(remove: bool) -> Result<()> {
    let _apartment = Apartment::new()?;
    let executable = std::env::current_exe()?;
    let server = format!("\"{}\"", executable.display());
    let class_key = format!("Software\\Classes\\CLSID\\{TOAST_ACTIVATOR_CLSID}");
    let server_key = format!("{class_key}\\LocalServer32");
    let app_key = format!("Software\\Classes\\AppUserModelId\\{APP_USER_MODEL_ID}");
    let shortcut = shortcut_path()?;
    if remove {
        if let Some(existing) = registered_executable(&server_key)?
            && !existing.eq_ignore_ascii_case(&server)
        {
            return Err(
                "Another Rustty location is registered; run --unregister from that copy.".into(),
            );
        }
        for path in [&class_key, &app_key] {
            let path = wide(path);
            let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr())) };
            if !matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
                status.ok()?;
            }
        }
        match std::fs::remove_file(&shortcut) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        println!("Removed Rustty's Start Menu and notification registration.");
    } else {
        std::fs::create_dir_all(shortcut.parent().ok_or("invalid Start Menu path")?)?;
        // Create the shortcut before exposing the COM activation path.
        save_shortcut(&shortcut, &executable)?;
        set_string(&server_key, "", &server)?;
        set_string(&app_key, "DisplayName", "Rustty")?;
        set_string(&app_key, "IconUri", &executable.display().to_string())?;
        set_string(&app_key, "CustomActivator", TOAST_ACTIVATOR_CLSID)?;
        println!("Registered {}", executable.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::STGM_READ;

    #[test]
    fn shortcut_retains_application_identity_and_toast_activator() {
        let _apartment = Apartment::new().unwrap();
        let path = std::env::temp_dir().join(format!("rustty-shortcut-{}.lnk", std::process::id()));
        let exe = std::env::current_exe().unwrap();
        save_shortcut(&path, &exe).unwrap();
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.cast::<IPersistFile>()
                .unwrap()
                .Load(PCWSTR(wide(&path).as_ptr()), STGM_READ)
                .unwrap();
            let store: IPropertyStore = link.cast().unwrap();
            let id = store.GetValue(&APP_ID).unwrap();
            assert_eq!(id.to_string(), APP_USER_MODEL_ID);
            let activator = store.GetValue(&ACTIVATOR).unwrap();
            assert!(!activator.is_empty());
        }
        std::fs::remove_file(path).unwrap();
    }
}
