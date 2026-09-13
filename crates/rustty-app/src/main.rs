#[cfg(target_os = "macos")]
mod desktop;

fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = desktop::run() {
        eprintln!("Rustty: {error}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("The Rustty desktop application currently supports macOS.");
        std::process::exit(1);
    }
}
