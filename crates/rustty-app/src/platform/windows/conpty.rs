//! Initialize the shared PTY backend with Rustty's verified bundled ConPTY.
use std::path::Path;

pub fn initialize(resources: &Path) -> Result<(), String> {
    // Loading the DLL alone does not initialize portable-pty's process-wide API.
    // Share tmuxx's initializer so both ordinary and embedded panes use the same
    // verified runtime, retained for as long as PTY workers can use it.
    let identity = rustty::tmuxx::initialize_conpty(resources).map_err(|error| {
        format!("{error}. Build or copy the complete Rustty folder, including resources.")
    })?;
    eprintln!(
        "Rustty PTY: {} (ConPTY {})",
        identity.directory.display(),
        identity.version
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_bundles_fail_before_loading_a_system_fallback() {
        let directory =
            std::env::temp_dir().join(format!("rustty-conpty-missing-{}", std::process::id()));
        let error = initialize(&directory).unwrap_err();
        assert!(error.contains("Missing bundled ConPTY"));
        assert!(error.contains("including resources"));
    }
}
