//! Portable local addresses resolve to the existing account-private endpoints.
use std::{
    io,
    path::{Path, PathBuf},
};

pub const LOCAL_TARGET_HINT: &str = "local:NAME";

pub fn local_endpoint(target: &str) -> io::Result<PathBuf> {
    if let Some(name) = target.strip_prefix("local:") {
        return tmuxx_cli::named_native_endpoint(name).and_then(std::path::absolute);
    }
    #[cfg(unix)]
    if let Some(path) = target.strip_prefix("unix:") {
        return std::path::absolute(path);
    }
    #[cfg(windows)]
    {
        if let Some(name) = target.strip_prefix("pipe:") {
            return rustty::tmuxx::windows_endpoint(name);
        }
        if target.starts_with(r"\\.\pipe\") {
            return Ok(target.into());
        }
    }
    #[cfg(unix)]
    if Path::new(target).is_absolute() {
        return Ok(target.into());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Use {LOCAL_TARGET_HINT}, ssh:HOST, or system-ssh:HOST"),
    ))
}

/// Only shorten an endpoint when the resulting address resolves back to it.
pub fn local_target(endpoint: &Path) -> String {
    if let Some(name) = tmuxx_cli::native_endpoint_name(endpoint) {
        return format!("local:{name}");
    }
    #[cfg(unix)]
    return format!("unix:{}", endpoint.display());
    #[cfg(not(unix))]
    endpoint.display().to_string()
}

pub fn local_label(endpoint: &Path) -> String {
    let name =
        tmuxx_cli::native_endpoint_name(endpoint).unwrap_or_else(|| endpoint.display().to_string());
    format!("local · {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_and_labels_round_trip() {
        for name in ["default", "work", "build 雪 & stuff"] {
            let target = format!("local:{name}");
            let endpoint = local_endpoint(&target).unwrap();
            assert_eq!(local_target(&endpoint), target);
            assert_eq!(local_label(&endpoint), format!("local · {name}"));
            assert_eq!(local_endpoint(&local_target(&endpoint)).unwrap(), endpoint);
        }
    }

    #[cfg(windows)]
    #[test]
    fn legacy_pipe_names_and_full_paths_keep_the_same_endpoint() {
        let endpoint = local_endpoint("pipe:work").unwrap();
        assert_eq!(local_endpoint("local:work").unwrap(), endpoint);
        assert_eq!(
            local_endpoint(endpoint.to_str().unwrap()).unwrap(),
            endpoint
        );
        // Names supported by the old Windows encoding remain usable even when
        // they cannot be expressed as a portable local name.
        let unusual = local_endpoint("pipe:work/legacy").unwrap();
        assert_eq!(local_target(&unusual), unusual.to_str().unwrap());
        assert_eq!(local_endpoint(&local_target(&unusual)).unwrap(), unusual);
    }

    #[cfg(unix)]
    #[test]
    fn custom_unix_paths_keep_their_address() {
        let endpoint = std::env::temp_dir().join("rustty-custom-local/socket.native");
        let target = format!("unix:{}", endpoint.display());
        assert_eq!(local_target(&endpoint), target);
        assert_eq!(local_endpoint(&target).unwrap(), endpoint);
        assert_eq!(
            local_endpoint(endpoint.to_str().unwrap()).unwrap(),
            endpoint
        );
        let named = local_endpoint("local:work").unwrap();
        assert_eq!(
            local_endpoint(&format!("unix:{}", named.display())).unwrap(),
            named
        );
    }
}
