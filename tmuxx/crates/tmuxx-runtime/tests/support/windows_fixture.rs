//! Explicit Windows applications for native runtime conformance fixtures.
pub const RAW_CONSOLE: &str = "import ctypes,msvcrt,os\nk=ctypes.WinDLL('kernel32',use_last_error=True)\nk.GetStdHandle.restype=ctypes.c_void_p\nk.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]\nassert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)\nassert k.SetConsoleMode(k.GetStdHandle(-10),0x200)\nassert k.SetConsoleMode(k.GetStdHandle(-11),0xD)\nmsvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)\n";

pub fn argv(args: &[&str]) -> Vec<Vec<u8>> {
    let mut values = args
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    let python = std::env::var("TMUXX_REMOTE_PYTHON").unwrap_or_else(|_| "python".into());
    let fixture = if args.ends_with(&["sleep", "30"]) {
        Some((2, "import time; time.sleep(30)".to_string()))
    } else if args.ends_with(&["sleep 30"]) {
        Some((1, "import time; time.sleep(30)".to_string()))
    } else if args.ends_with(&["/bin/sh", "-c", "stty raw -echo; printf READY; exec cat"]) {
        Some((
            3,
            format!(
                "{RAW_CONSOLE}os.write(1,b'READY')\nwhile data:=os.read(0,4096): os.write(1,data)"
            ),
        ))
    } else {
        None
    };
    if let Some((count, script)) = fixture {
        values.truncate(values.len() - count);
        values.extend([python.clone(), "-u".into(), "-c".into(), script]);
    }
    if let Some(index) = values.iter().position(|value| value == "python3") {
        values[index] = python;
        let source = values.last_mut().unwrap();
        *source = source
            .replace("import os,tty\ntty.setraw(0)\n", RAW_CONSOLE)
            .replace("os.get_terminal_size(0)", "os.get_terminal_size(1)");
    }
    values.into_iter().map(String::into_bytes).collect()
}
