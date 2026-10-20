//! Verify and hold the application's pinned ConPTY runtime before creating a PTY.
//!
//! The retained file handles deny writes and deletion, so later pane creation
//! cannot run a substituted OpenConsole executable after initial validation.
use serde::Deserialize;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};

const MANIFEST: &str = include_str!("../../../windows/conpty.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConptyIdentity {
    pub version: String,
    pub architecture: String,
    pub package_sha256: String,
    pub directory: PathBuf,
    pub conpty_sha256: String,
    pub openconsole_sha256: String,
}

#[derive(Deserialize)]
struct Package {
    version: String,
    architecture: String,
    sha256: String,
    files: Vec<Member>,
}

#[derive(Deserialize)]
struct Member {
    name: String,
    length: u64,
    sha256: String,
}

struct Loaded {
    identity: ConptyIdentity,
    _files: Vec<File>,
}

static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

/// Initialize the process-wide PTY API using `resources/conpty` exclusively.
///
/// Missing, altered and incompatible resources fail before any PTY is opened.
/// Repeated calls must name the same canonical resources directory. A different
/// runtime, including a previously initialized system runtime, is an error.
pub fn initialize_conpty(resources: &Path) -> io::Result<ConptyIdentity> {
    let mut loaded = LOADED
        .lock()
        .map_err(|_| io::Error::other("ConPTY initialization lock poisoned"))?;
    let directory = resources.join("conpty").canonicalize().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "Missing bundled ConPTY directory {}: {error}",
                resources.join("conpty").display()
            ),
        )
    })?;
    if let Some(loaded) = &*loaded {
        if loaded.identity.directory != directory {
            return Err(io::Error::other(
                "ConPTY is already initialized from another resources directory",
            ));
        }
        return Ok(loaded.identity.clone());
    }
    let package: Package = serde_json::from_str(MANIFEST).map_err(io::Error::other)?;
    if package.architecture != std::env::consts::ARCH {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The bundled ConPTY package requires Windows x86_64",
        ));
    }
    let mut files = Vec::new();
    for member in &package.files {
        let path = directory
            .join(&member.name)
            .canonicalize()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "Cannot resolve bundled ConPTY resource {}: {error}",
                        directory.join(&member.name).display()
                    ),
                )
            })?;
        if path.parent() != Some(directory.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ConPTY resources must be regular files in the same runtime directory",
            ));
        }
        // FILE_SHARE_READ only: preserve verified bytes for the process lifetime,
        // including OpenConsole launches that happen long after initialization.
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "Cannot open bundled ConPTY runtime {}: {error}",
                        path.display()
                    ),
                )
            })?;
        if file.metadata()?.len() != member.length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Incompatible bundled ConPTY resource {}: unexpected length",
                    path.display()
                ),
            ));
        }
        let mut bytes = Vec::with_capacity(member.length as usize);
        file.read_to_end(&mut bytes)?;
        if sha256(&bytes)? != member.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Incompatible bundled ConPTY resource {}: SHA-256 mismatch",
                    path.display()
                ),
            ));
        }
        files.push(file);
    }
    portable_pty::win::conpty::initialize_bundled(&directory.join("conpty.dll"))
        .map_err(io::Error::other)?;
    let identity = ConptyIdentity {
        version: package.version,
        architecture: package.architecture,
        package_sha256: package.sha256,
        directory,
        conpty_sha256: package.files[0].sha256.clone(),
        openconsole_sha256: package.files[1].sha256.clone(),
    };
    *loaded = Some(Loaded {
        identity: identity.clone(),
        _files: files,
    });
    Ok(identity)
}

fn sha256(bytes: &[u8]) -> io::Result<String> {
    let count = u32::try_from(bytes.len())
        .map_err(|_| io::Error::other("ConPTY resource exceeds hash input limit"))?;
    let mut digest = [0u8; 32];
    // The SHA-256 pseudo-handle is process independent and needs no closing.
    // All buffers remain valid for the duration of this synchronous call.
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            std::ptr::null(),
            0,
            bytes.as_ptr(),
            count,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "BCryptHash failed: NTSTATUS {status:#x}"
        )));
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("tmuxx-conpty-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn conpty_sha256_uses_native_sha256() {
        assert_eq!(
            sha256(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn missing_conpty_resources_fail_before_system_initialization() {
        let directory = fixture_root();
        let result = initialize_conpty(&directory);
        std::fs::remove_dir(directory).unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing bundled ConPTY")
        );
    }

    #[test]
    fn incompatible_conpty_resources_fail_before_system_initialization() {
        let directory = fixture_root();
        let runtime = directory.join("conpty");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::write(runtime.join("conpty.dll"), b"an incompatible DLL").unwrap();
        let wrong_length = initialize_conpty(&directory);
        let package: Package = serde_json::from_str(MANIFEST).unwrap();
        std::fs::write(
            runtime.join("conpty.dll"),
            vec![0; package.files[0].length as usize],
        )
        .unwrap();
        let wrong_digest = initialize_conpty(&directory);
        std::fs::remove_file(runtime.join("conpty.dll")).unwrap();
        std::fs::remove_dir(runtime).unwrap();
        std::fs::remove_dir(directory).unwrap();
        assert!(
            wrong_length
                .unwrap_err()
                .to_string()
                .contains("Incompatible bundled ConPTY resource")
        );
        assert!(
            wrong_digest
                .unwrap_err()
                .to_string()
                .contains("SHA-256 mismatch")
        );
    }
}
