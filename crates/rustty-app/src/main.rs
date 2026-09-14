#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod desktop;
#[cfg(target_os = "windows")]
mod windows_registration;

fn main() {
    #[cfg(target_os = "windows")]
    attach_parent_console();

    #[cfg(target_os = "windows")]
    if std::env::args().len() == 2
        && std::env::args()
            .nth(1)
            .is_some_and(|arg| arg.eq_ignore_ascii_case("-Embedding"))
    {
        if let Err(error) = rustty_app::platform::run_toast_activator() {
            eprintln!("Rustty notification activation: {error}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(target_os = "windows")]
    if let Some(argument) = std::env::args().nth(1)
        && matches!(argument.as_str(), "--register" | "--unregister")
    {
        if let Err(error) = windows_registration::run(argument == "--unregister") {
            eprintln!("Rustty: {error}");
            show_startup_error(&error.to_string());
            std::process::exit(1);
        }
        return;
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Err(error) = desktop::run() {
        eprintln!("Rustty: {error}");
        #[cfg(target_os = "windows")]
        show_startup_error(&error.to_string());
        std::process::exit(1);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        eprintln!("The Rustty desktop application supports macOS and Windows.");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // A release GUI executable should print CLI diagnostics in its invoking
    // terminal, without allocating a console when launched from Explorer.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(target_os = "windows")]
fn show_startup_error(message: &str) {
    use windows::Win32::{
        System::Console::GetConsoleWindow,
        UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
    };
    // Automated checks and terminal launches already have stderr. Explorer
    // launches need a visible explanation when startup fails before a window.
    if unsafe { GetConsoleWindow() }.0.is_null() && std::env::var_os("RUSTTY_SMOKE_DIR").is_none() {
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        unsafe {
            MessageBoxW(
                None,
                windows::core::PCWSTR(text.as_ptr()),
                windows::core::w!("Rustty"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
