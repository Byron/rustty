use super::error;
use crate::config::Command;
use portable_pty::CommandBuilder;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(any(windows, test))]
#[path = "windows_shell.rs"]
mod windows_shell;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellKind {
    GitBash,
    Bash,
    PowerShell,
    Cmd,
    Posix,
    Other,
}

#[derive(Debug)]
struct DirectoryCache {
    report: String,
    path: Option<PathBuf>,
}

/// The launched shell's identity, independent of configuration provenance.
#[derive(Clone, Debug)]
pub struct ShellInfo {
    pub(super) argv: Vec<String>,
    source: String,
    diagnostic: Option<String>,
    kind: ShellKind,
    cygpath: Option<PathBuf>,
    directory_cache: Arc<Mutex<Option<DirectoryCache>>>,
}

impl fmt::Display for ShellInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} — {}", self.program(), self.source)
    }
}

impl ShellInfo {
    pub fn program(&self) -> &str {
        self.argv.first().map_or("", String::as_str)
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
    pub fn kind(&self) -> &ShellKind {
        &self.kind
    }

    pub(super) fn new(argv: Vec<String>, source: String) -> Self {
        let program = argv.first().map_or("", String::as_str);
        let resolved = find_program(program).unwrap_or_else(|| PathBuf::from(program));
        let name = resolved
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let cygpath = (cfg!(windows) && name == "bash")
            .then(|| {
                let directory = resolved.parent()?;
                [
                    directory.join("cygpath.exe"),
                    directory.parent()?.join("usr/bin/cygpath.exe"),
                ]
                .into_iter()
                .find(|path| path.is_file())
            })
            .flatten();
        let kind = match name.as_str() {
            "bash" if cygpath.is_some() => ShellKind::GitBash,
            "bash" => ShellKind::Bash,
            "powershell" | "pwsh" => ShellKind::PowerShell,
            "cmd" => ShellKind::Cmd,
            "sh" | "zsh" | "fish" | "dash" | "ksh" => ShellKind::Posix,
            _ => ShellKind::Other,
        };
        Self {
            argv,
            source,
            diagnostic: None,
            kind,
            cygpath,
            directory_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn warn(&mut self, message: impl AsRef<str>) {
        if let Some(previous) = &mut self.diagnostic {
            previous.push_str("; ");
            previous.push_str(message.as_ref());
        } else {
            self.diagnostic = Some(message.as_ref().to_owned());
        }
    }

    pub(super) fn path_for_shell(&self, path: &Path) -> io::Result<OsString> {
        if let Some(cygpath) = &self.cygpath {
            Ok(translate(cygpath, "-u", path)?.into())
        } else {
            Ok(path.as_os_str().to_owned())
        }
    }

    /// Shell-quoted dropped-file input, including the trailing argument separator.
    pub fn quote_path(&self, path: &Path) -> io::Result<Vec<u8>> {
        let path = self.path_for_shell(path)?;
        let path = path.to_string_lossy();
        let quoted = match self.kind {
            ShellKind::PowerShell => format!("'{}' ", path.replace('\'', "''")),
            ShellKind::Cmd => {
                // CMD expands these even inside double quotes. Refuse an unsafe
                // insertion rather than substitute an environment variable.
                if path.contains(['%', '!', '"', '\r', '\n']) {
                    return Err(error(
                        "CMD cannot insert this path literally; paste it with environment expansion disabled",
                    ));
                }
                format!("\"{path}\" ")
            }
            _ => format!("'{}' ", path.replace('\'', "'\\''")),
        };
        Ok(quoted.into_bytes())
    }

    /// Decode native, OSC 7 file-URI, kitty shell, and OSC 9;9 cwd reports.
    /// Git paths are translated by that Git installation, including mount roots.
    pub fn directory_from_report(&self, report: &str) -> Option<PathBuf> {
        if let Ok(cache) = self.directory_cache.lock()
            && let Some(previous) = cache.as_ref()
            && previous.report == report
        {
            return previous.path.clone();
        }
        let result = self.decode_directory(report);
        if let Ok(mut cache) = self.directory_cache.lock() {
            *cache = Some(DirectoryCache {
                report: report.to_owned(),
                path: result.clone(),
            });
        }
        result
    }

    fn decode_directory(&self, report: &str) -> Option<PathBuf> {
        let report = report
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(report);
        if report.is_empty() || report.contains('\0') {
            return None;
        }
        let (value, authority) = if let Some(uri) = report.strip_prefix("file://") {
            let (host, path) = uri.split_once('/')?;
            (percent_decode(&format!("/{path}"))?, Some(host))
        } else if let Some(uri) = report.strip_prefix("kitty-shell-cwd://") {
            let (_, path) = uri.split_once('/')?;
            (format!("/{path}"), None)
        } else {
            (report.to_owned(), None)
        };
        if value.contains('\0') {
            return None;
        }
        #[cfg(windows)]
        let value = {
            let mut value = value;
            // file:///C:/... is an absolute Windows path after removing its URI slash.
            if authority.is_some() && value.as_bytes().get(2) == Some(&b':') {
                value.remove(0);
            } else if let Some(host) = authority
                && !host.is_empty()
                && !host.eq_ignore_ascii_case("localhost")
                && !std::env::var("COMPUTERNAME").is_ok_and(|name| name.eq_ignore_ascii_case(host))
            {
                value = format!(r"\\{}{}", host, value.replace('/', r"\"));
            }
            if let Some(cygpath) = &self.cygpath
                && value.starts_with('/')
            {
                value = translate(cygpath, "-w", Path::new(&value)).ok()?;
            }
            value
        };
        #[cfg(not(windows))]
        let _ = authority;
        let path = PathBuf::from(value);
        path.is_absolute().then_some(path)
    }

    #[cfg(windows)]
    pub(super) fn command_text(&self, text: &str) -> io::Result<CommandBuilder> {
        let mut builder = CommandBuilder::new(self.program());
        match self.kind {
            ShellKind::GitBash | ShellKind::Bash | ShellKind::Posix => {
                // Keep the inherited login preference without forcing interactive
                // flags onto a command that must terminate.
                if self
                    .argv
                    .iter()
                    .skip(1)
                    .any(|arg| matches!(arg.as_str(), "-l" | "--login"))
                {
                    builder.arg("-l");
                }
                builder.args(["-c", text]);
            }
            ShellKind::PowerShell => builder.args(["-NoLogo", "-Command", text]),
            ShellKind::Cmd => builder.args(["/d", "/s", "/c", text]),
            ShellKind::Other => {
                return Err(error(
                    "the inherited shell has unknown command-string syntax; use -e with explicit arguments",
                ));
            }
        }
        Ok(builder)
    }
}

/// Report the shell selected for a default session without launching it.
pub fn default_shell() -> ShellInfo {
    #[cfg(windows)]
    {
        let resolved = windows_shell::resolve().and_then(|resolved| {
            if find_program(&resolved.argv[0]).is_none() {
                return Err(error(format!(
                    "Windows Terminal shell executable was not found: {}",
                    resolved.argv[0]
                )));
            }
            Ok(resolved)
        });
        match resolved {
            Ok(resolved) => ShellInfo::new(resolved.argv, resolved.source),
            Err(error) => {
                let program = std::env::var("ComSpec")
                    .ok()
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "cmd.exe".into());
                let mut shell = ShellInfo::new(vec![program.clone()], "ComSpec fallback".into());
                shell.warn(format!(
                    "Could not inherit the Windows Terminal default shell: {error}; using {program}"
                ));
                shell
            }
        }
    }
    #[cfg(not(windows))]
    ShellInfo::new(
        vec![CommandBuilder::new_default_prog().get_shell()],
        "system login shell".into(),
    )
}

/// Explicit argv takes precedence over Windows Terminal, without reading its files.
pub fn resolve_shell(command: Option<&Command>) -> ShellInfo {
    match command {
        Some(Command::Direct(argv)) => {
            ShellInfo::new(argv.clone(), "explicit Rustty command".into())
        }
        _ => default_shell(),
    }
}

fn find_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_file() {
        return Some(path.to_owned());
    }
    if path.is_absolute() || path.components().count() > 1 {
        return None;
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = directory.join(program);
        if path.is_file() {
            return Some(path);
        }
        #[cfg(windows)]
        if path.extension().is_none() {
            let path = path.with_extension("exe");
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn translate(cygpath: &Path, mode: &str, path: &Path) -> io::Result<String> {
    let mut command = std::process::Command::new(cygpath);
    command
        .args([mode, "--"])
        .arg(path)
        .env("LC_ALL", "C.UTF-8");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(error(format!(
            "Git path conversion failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(error)
}

fn percent_decode(text: &str) -> Option<String> {
    let mut input = text.bytes();
    let mut result = Vec::new();
    while let Some(byte) = input.next() {
        result.push(if byte == b'%' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(result).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_argv_never_uses_terminal_defaults() {
        let shell = resolve_shell(Some(&Command::Direct(vec![
            "private-shell.exe".into(),
            "argument".into(),
        ])));
        assert_eq!(shell.program(), "private-shell.exe");
        assert_eq!(shell.source(), "explicit Rustty command");
        assert!(shell.diagnostic().is_none());
    }
    #[test]
    fn quoting_tracks_shell_syntax() {
        let bash = ShellInfo::new(vec!["/bin/bash".into()], "test".into());
        let powershell = ShellInfo::new(vec!["pwsh.exe".into()], "test".into());
        let cmd = ShellInfo::new(vec!["cmd.exe".into()], "test".into());
        assert_eq!(
            bash.quote_path(Path::new("one's file")).unwrap(),
            b"'one'\\''s file' "
        );
        assert_eq!(
            powershell.quote_path(Path::new("one's file")).unwrap(),
            b"'one''s file' "
        );
        assert_eq!(
            cmd.quote_path(Path::new("one & two")).unwrap(),
            b"\"one & two\" "
        );
        assert!(cmd.quote_path(Path::new("%PATH%")).is_err());
    }
    #[test]
    fn cwd_reports_decode_unicode_and_reject_invalid_sequences() {
        let shell = ShellInfo::new(vec!["cmd.exe".into()], "test".into());
        #[cfg(windows)]
        {
            assert_eq!(
                shell.directory_from_report("file:///C:/Users/one%20two"),
                Some(PathBuf::from("C:/Users/one two"))
            );
            assert_eq!(
                shell.directory_from_report(r"C:\Users\one"),
                Some(PathBuf::from(r"C:\Users\one"))
            );
            assert_eq!(
                shell.directory_from_report("file://server/share/a"),
                Some(PathBuf::from(r"\\server\share\a"))
            );
        }
        #[cfg(not(windows))]
        assert_eq!(
            shell.directory_from_report("file://localhost/one%20two"),
            Some(PathBuf::from("/one two"))
        );
        for invalid in ["relative", "file://host", "file:///bad%0", "file:///bad%00"] {
            assert_eq!(shell.directory_from_report(invalid), None);
        }
    }

    #[cfg(windows)]
    #[test]
    fn git_bash_mount_paths_and_dropped_files_use_its_own_cygpath() {
        let Some(program_files) = std::env::var_os("ProgramFiles") else {
            return;
        };
        let git = PathBuf::from(program_files).join("Git");
        let bash = git.join("bin/bash.exe");
        if !bash.is_file() {
            eprintln!("Git Bash path integration check skipped: Git is not installed");
            return;
        }
        let shell = ShellInfo::new(vec![bash.to_string_lossy().into_owned()], "test".into());
        assert_eq!(shell.kind(), &ShellKind::GitBash);
        assert_eq!(
            shell.directory_from_report("kitty-shell-cwd://host/usr/bin"),
            Some(git.join("usr/bin"))
        );
        let quoted = String::from_utf8(shell.quote_path(&git.join("one's file")).unwrap()).unwrap();
        assert!(quoted.starts_with("'/"), "{quoted}");
        assert!(quoted.ends_with("/one'\\''s file' "), "{quoted}");
        assert!(!quoted.contains("C:"), "{quoted}");
    }
}
