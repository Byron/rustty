//! Native application directories shared by configuration, sessions and hosts.
use std::env;
use std::io;
use std::path::PathBuf;

pub fn home_dir() -> io::Result<PathBuf> {
    env::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable"))
}

fn environment_dir(name: &str) -> io::Result<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{name} is unavailable")))
}

/// The directory containing this user's Rustty configuration.
pub fn config_dir() -> io::Result<PathBuf> {
    if cfg!(windows) {
        Ok(environment_dir("APPDATA")?.join("Rustty"))
    } else if cfg!(target_os = "macos") {
        Ok(home_dir()?.join("Library/Application Support/com.rustty.app"))
    } else {
        Ok(environment_dir("XDG_CONFIG_HOME")
            .unwrap_or(home_dir()?.join(".config"))
            .join("rustty"))
    }
}

/// The directory containing local workspace state; it need not roam with settings.
pub fn data_dir() -> io::Result<PathBuf> {
    if cfg!(windows) {
        Ok(environment_dir("LOCALAPPDATA")?.join("Rustty"))
    } else if cfg!(target_os = "macos") {
        config_dir()
    } else {
        Ok(environment_dir("XDG_STATE_HOME")
            .unwrap_or(home_dir()?.join(".local/state"))
            .join("rustty"))
    }
}
