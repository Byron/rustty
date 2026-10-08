//! A disposable foreground reporter using the same protocol as real agents.
use rustty::vt::agent::{Event, Snapshot, State};
use std::{
    io::{self, IsTerminal},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const HELP: &str = "Usage: rustty agent-sim [--state STATE] [--interval SECONDS] [--raw]

Without flags, cycle idle → working → needs_input → done → error → paused → unknown
every 3 seconds. --state repeats one permanent state instead.
--interval sets the reporting interval (0.05–86400 seconds, decimals accepted).
States: idle, working, needs_input, done, error, paused, unknown.
Ctrl-C exits and unregisters. On macOS, Ctrl-Z unregisters until resumed.
Output must be this process's terminal stdout; --raw permits explicit fixtures.
This simulates status only: no agent work, model calls or direct settings/file edits.";

const STATES: [State; 7] = [
    State::Idle,
    State::Working,
    State::NeedsInput,
    State::Done,
    State::Error,
    State::Paused,
    State::Unknown,
];

struct Options {
    state: Option<State>,
    interval: Duration,
    raw: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut args = args.into_iter().peekable();
    if args
        .peek()
        .is_some_and(|s| matches!(s.as_str(), "--help" | "-h"))
    {
        args.next();
        return if args.next().is_none() {
            Ok(None)
        } else {
            Err(HELP.into())
        };
    }
    let mut state = None;
    let mut interval = None;
    let mut raw = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--state" if state.is_none() => {
                state = Some(args.next().ok_or("missing --state value")?.parse()?);
            }
            "--interval" if interval.is_none() => {
                let seconds = args
                    .next()
                    .ok_or("missing --interval value")?
                    .parse::<f64>()
                    .map_err(|_| "invalid --interval seconds")?;
                if !(0.05..=86400.0).contains(&seconds) {
                    return Err("--interval must be between 0.05 and 86400 seconds".into());
                }
                interval = Some(Duration::from_secs_f64(seconds));
            }
            "--raw" if !raw => raw = true,
            _ => {
                return Err(format!(
                    "unexpected or repeated agent-sim argument: {arg}\n{HELP}"
                ));
            }
        }
    }
    Ok(Some(Options {
        state,
        interval: interval.unwrap_or(Duration::from_secs(3)),
        raw,
    }))
}

fn snapshot(state: Option<State>, tick: u64, process: u32) -> Snapshot {
    let selected = state.unwrap_or(STATES[(tick % STATES.len() as u64) as usize]);
    Snapshot {
        state: selected,
        label: Some("Agent sim 🦀".into()),
        thread_id: Some(format!("sim-{process}")),
        // Repeating a fixed Done state must not manufacture unread completions.
        turn_id: (selected == State::Done).then(|| {
            format!(
                "turn-{}",
                if state.is_some() {
                    1
                } else {
                    tick / STATES.len() as u64 + 1
                }
            )
        }),
    }
}

static STOP: AtomicBool = AtomicBool::new(false);
static SUSPEND: AtomicBool = AtomicBool::new(false);

pub fn run(args: impl IntoIterator<Item = String>) -> Result<(), String> {
    let Some(options) = parse(args)? else {
        println!("{HELP}");
        return Ok(());
    };
    let stdout = io::stdout();
    if !stdout.is_terminal() && !options.raw {
        return Err("agent-sim stdout is not a terminal; use --raw for explicit fixtures".into());
    }
    let _signals = signals::install()?;
    let mut output = stdout.lock();
    let mut send = |event: Event| {
        super::agent_cli::emit(&mut output, &event.frame()?, true, options.raw)
            .map_err(|error| error.to_string())
    };
    let process = std::process::id();
    let mut tick = 0;
    send(Event::Begin(snapshot(options.state, tick, process)))?;
    let mut next = Instant::now() + options.interval;
    while !STOP.load(Ordering::Relaxed) {
        if SUSPEND.swap(false, Ordering::Relaxed) {
            send(Event::End)?;
            signals::suspend();
            if STOP.load(Ordering::Relaxed) {
                return Ok(());
            }
            send(Event::Begin(snapshot(options.state, tick, process)))?;
            next = Instant::now() + options.interval;
        }
        let now = Instant::now();
        if now >= next {
            tick = tick.wrapping_add(1);
            send(Event::Update(snapshot(options.state, tick, process)))?;
            next = Instant::now() + options.interval;
        }
        std::thread::sleep(
            next.saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25)),
        );
    }
    send(Event::End)
}

#[cfg(target_os = "macos")]
mod signals {
    use super::*;
    pub struct Guard(Vec<(libc::c_int, libc::sighandler_t)>);
    extern "C" fn receive(signal: libc::c_int) {
        // Signal handlers never write terminal output or acquire locks.
        if signal == libc::SIGTSTP {
            &SUSPEND
        } else {
            &STOP
        }
        .store(true, Ordering::Relaxed);
    }
    pub fn install() -> Result<Guard, String> {
        let mut guard = Guard(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGTSTP] {
            // The standalone CLI owns its process signal handlers until it returns.
            let previous =
                unsafe { libc::signal(signal, receive as *const () as libc::sighandler_t) };
            if previous == libc::SIG_ERR {
                return Err(io::Error::last_os_error().to_string());
            }
            guard.0.push((signal, previous));
        }
        Ok(guard)
    }
    pub fn suspend() {
        // End is already flushed; SIGCONT resumes at the following statement.
        unsafe { libc::raise(libc::SIGSTOP) };
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            for &(signal, previous) in &self.0 {
                unsafe { libc::signal(signal, previous) };
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod signals {
    use super::*;
    use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
    use windows::core::BOOL;
    pub struct Guard;
    unsafe extern "system" fn receive(event: u32) -> BOOL {
        let handled = matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT);
        if handled {
            STOP.store(true, Ordering::Relaxed);
        }
        handled.into()
    }
    pub fn install() -> Result<Guard, String> {
        unsafe { SetConsoleCtrlHandler(Some(receive), true) }.map_err(|error| error.to_string())?;
        Ok(Guard)
    }
    pub fn suspend() {}
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = unsafe { SetConsoleCtrlHandler(Some(receive), false) };
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod signals {
    pub fn install() -> Result<(), String> {
        Err("agent-sim currently supports macOS and Windows".into())
    }
    pub fn suspend() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options(args: &[&str]) -> Result<Option<Options>, String> {
        parse(args.iter().map(|s| s.to_string()))
    }
    #[test]
    fn defaults_fixed_state_and_interval_validation() {
        let defaults = options(&[]).unwrap().unwrap();
        assert_eq!(defaults.state, None);
        assert_eq!(defaults.interval, Duration::from_secs(3));
        assert!(!defaults.raw);
        let fixed = options(&["--state", "needs_input", "--interval", "0.1", "--raw"])
            .unwrap()
            .unwrap();
        assert_eq!(fixed.state, Some(State::NeedsInput));
        assert_eq!(fixed.interval, Duration::from_millis(100));
        assert!(fixed.raw);
        assert!(options(&["--help"]).unwrap().is_none());
        for args in [
            vec!["--state"],
            vec!["--state", "waiting"],
            vec!["--state", "idle", "--state", "done"],
            vec!["--interval"],
            vec!["--interval", "NaN"],
            vec!["--interval", "inf"],
            vec!["--interval", "0"],
            vec!["--interval", "-1"],
            vec!["--interval", "0.001"],
            vec!["--interval", "86401"],
            vec!["--interval", "1", "--interval", "2"],
            vec!["--raw", "--raw"],
            vec!["--help", "extra"],
        ] {
            assert!(options(&args).is_err(), "{args:?}");
        }
    }
    #[test]
    fn rotating_completions_are_new_but_fixed_done_repeats_the_same_identity() {
        for tick in 0..14 {
            let report = snapshot(None, tick, 42);
            assert_eq!(report.state, STATES[(tick % 7) as usize]);
            assert!(Event::Begin(report.clone()).frame().is_ok());
            assert_eq!(report.turn_id.is_some(), report.state == State::Done);
        }
        assert_ne!(
            snapshot(None, 3, 42).turn_id,
            snapshot(None, 10, 42).turn_id
        );
        assert_eq!(
            snapshot(Some(State::Done), 0, 42),
            snapshot(Some(State::Done), 100, 42)
        );
        assert_ne!(
            snapshot(None, 0, 42).thread_id,
            snapshot(None, 0, 43).thread_id
        );
    }
}
