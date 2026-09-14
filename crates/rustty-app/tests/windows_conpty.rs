//! Verify DEC 2026 across the real PTY, not only by feeding the parser directly.
//! Build the debug bundle, then cargo test -p rustty-app --test windows_conpty -- --ignored.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("windows_conpty: ignored (build the Windows bundle, then pass --ignored)");
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    windows::run()?;
    Ok(())
}

#[cfg(target_os = "windows")]
mod windows {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use rustty::vt::{Effect, Terminal};
    use std::{
        io::{Read, Write},
        path::{Path, PathBuf},
        sync::mpsc,
        time::{Duration, Instant},
    };

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let resources =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/Rustty/resources");
        // --system demonstrates the regression against the unpatched OS runtime
        // in a separate process; DLL selection is fixed for the process lifetime.
        if !std::env::args().any(|arg| arg == "--system") {
            rustty_app::platform::initialize_terminal_runtime(&resources)?;
        }
        let pair = native_pty_system().openpty(PtySize {
            cols: 80,
            rows: 24,
            ..Default::default()
        })?;
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let shell = std::env::var_os("RUSTTY_SMOKE_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()),
                )
                .join("Git/bin/bash.exe")
            });
        let mut command = CommandBuilder::new(shell);
        command.args(["--noprofile", "--norc", "-c", r"printf '\033[2J\033[Hstable frame\033[3;3H\033[?25h'; sleep .1; for i in {1..8}; do printf '\033[?2026h\033[?25l\033[2;1H\033[2K'; sleep .015; printf 'star %s' $i; sleep .015; printf '\033[3;3H\033[?25h\033[?2026l'; sleep .025; done"]);
        let mut child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let mut killer = child.clone_killer();
        let owner = std::thread::spawn(move || {
            let result = child.wait();
            drop(pair.master);
            result
        });
        let (sender, receiver) = mpsc::channel();
        let read = std::thread::spawn(move || {
            let mut buffer = [0; 32768];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(length) => {
                        if sender.send(Ok(buffer[..length].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut terminal = Terminal::new(80, 24, 0);
        let mut completed = 0;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            loop {
                let bytes = match receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(bytes) => bytes?,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(error) => return Err(error.into()),
                };
                // Every possible read split must leave the terminal synchronized
                // until BOTH the star and restored cursor have arrived.
                for byte in bytes {
                    let synchronized = terminal.modes.dec(2026);
                    for effect in terminal.feed(&[byte]) {
                        if let Effect::Write(reply) = effect {
                            writer.write_all(&reply)?;
                        }
                    }
                    if synchronized && !terminal.modes.dec(2026) {
                        completed += 1;
                        let screen = terminal.screen();
                        if screen.row_text(screen.row(0)) != "stable frame"
                            || screen.row_text(screen.row(1)) != format!("star {completed}")
                            || !screen.cursor.visible
                            || (screen.cursor.row, screen.cursor.col) != (2, 2)
                        {
                            return Err(format!("batch {completed} ended before its screen/cursor update: text={:?}, cursor=({}, {}, {})", screen.row_text(screen.row(1)), screen.cursor.row, screen.cursor.col, screen.cursor.visible).into());
                        }
                    }
                }
            }
            if completed != 8 {
                return Err(format!("received {completed} completed batches, expected 8").into());
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = killer.kill();
        }
        drop(writer);
        let status = owner.join().map_err(|_| "PTY owner panicked")??;
        read.join().map_err(|_| "PTY reader panicked")?;
        result?;
        if !status.success() {
            return Err(format!("fixture exited: {status}").into());
        }
        println!(
            "windows_conpty: 8 synchronized redraws preserved text and cursor ordering at every byte boundary"
        );
        Ok(())
    }
}
