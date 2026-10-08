use rustty::vt::agent::{Event, Snapshot};
use std::io::{self, IsTerminal, Write};

const HELP: &str = "Usage:
  rustty agent-status begin|update STATE [--label TEXT] [--thread-id ID] [--turn-id ID] [--raw]
  rustty agent-status end [--raw]

States: idle, working, needs_input, done, error, paused, unknown.
done requires --turn-id. Every begin/update replaces the complete snapshot.
Output must be this process's terminal stdout. --raw permits captured fixture output.
One serialized reporter may own a pane; always emit end on normal cleanup.";

const DECK_LEGEND: &str = "Rustty Stream Deck legend (5 × 3)

Agent keys: large status icon, with a short worktree task slug below when available.
For example, gitoxide.foo-bar becomes foo-bar. Long slugs are shortened to fit.

  ▂█▅  Working       Blue activity bars
   !   Needs input   Amber; a pending question does not mean all work stopped
   ✓   Done          Green; a completed turn ready to inspect, not proof of success
   ●   Idle          Neutral dot; also a locally acknowledged completion
   ×   Error         Red cross
   Ⅱ   Paused        Neutral pause bars
   ○   Unknown       Neutral hollow circle; insufficient or lost status
   —   Inactive      Dim dash, underlined when reserved; no focus target
   ⇄   Move          Purple arrows/outline; select the swap destination

A small mint line at the top marks the focused pane, independently of status.

Fixed board:
  Agent 1   Agent 2   Agent 3       ! Input     ▂█▅ Working
  Agent 4   Agent 5   Agent 6       ✓ Done       ● Idle
  Agent 7   Agent 8   Agent 9       ☀ Light      › Page

The four state keys cycle matching panes across all windows, tabs and pages.
Their numbers count matches; an empty group is dimmed and does nothing.
Light shows the selected brightness: 100 → 25 → 50 → 75 → 100%.
Page shows current/total; tap to advance and wrap.

Tap an agent to reveal its exact pane. Revealing Done acknowledges it locally.
Hold an agent for two seconds, then press another agent slot to swap or move it.
Press the source again to cancel. Empty/reserved destinations accept swaps.
In move mode Page and Light still work; the four state-cycle keys are disabled.
Keys never approve requests or send input to an agent.

With no reporting sessions the RUS/TTY/🦀💻✨/TTY/RUS board is decorative and inert.
This command prints a reference only; it does not connect to the USB device.
";

pub fn legend(args: impl IntoIterator<Item = String>) -> Result<(), String> {
    let mut args = args.into_iter();
    if args
        .next()
        .is_some_and(|arg| !matches!(arg.as_str(), "--help" | "-h"))
        || args.next().is_some()
    {
        return Err("Usage: rustty stream-deck-legend".into());
    }
    io::stdout()
        .lock()
        .write_all(DECK_LEGEND.as_bytes())
        .map_err(|error| error.to_string())
}

enum Command {
    Help,
    Emit { frame: Vec<u8>, raw: bool },
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let operation = args.next().ok_or(HELP)?;
    if matches!(operation.as_str(), "--help" | "-h") && args.next().is_none() {
        return Ok(Command::Help);
    }
    let mut snapshot = match operation.as_str() {
        "begin" | "update" => Some(Snapshot {
            state: args.next().ok_or("missing state")?.parse()?,
            label: None,
            thread_id: None,
            turn_id: None,
        }),
        "end" => None,
        _ => {
            return Err(format!(
                "unknown agent-status operation: {operation}\n{HELP}"
            ));
        }
    };
    let mut raw = false;
    while let Some(option) = args.next() {
        if option == "--raw" {
            if raw {
                return Err("duplicate --raw".into());
            }
            raw = true;
            continue;
        }
        let field = match (option.as_str(), snapshot.as_mut()) {
            ("--label", Some(snapshot)) => &mut snapshot.label,
            ("--thread-id", Some(snapshot)) => &mut snapshot.thread_id,
            ("--turn-id", Some(snapshot)) => &mut snapshot.turn_id,
            _ => return Err(format!("unexpected agent-status argument: {option}")),
        };
        if field.is_some() {
            return Err(format!("duplicate {option}"));
        }
        *field = Some(
            args.next()
                .ok_or_else(|| format!("missing value for {option}"))?,
        );
    }
    let event = match (operation.as_str(), snapshot) {
        ("begin", Some(snapshot)) => Event::Begin(snapshot),
        ("update", Some(snapshot)) => Event::Update(snapshot),
        ("end", None) => Event::End,
        _ => unreachable!(),
    };
    Ok(Command::Emit {
        frame: event.frame()?,
        raw,
    })
}

pub(super) fn emit(
    output: &mut impl Write,
    frame: &[u8],
    terminal: bool,
    raw: bool,
) -> io::Result<()> {
    if !terminal && !raw {
        return Err(io::Error::other(
            "stdout is not a terminal; use --raw only for explicit fixtures/passthrough",
        ));
    }
    output.write_all(frame)?;
    output.flush()
}

pub fn run(args: impl IntoIterator<Item = String>) -> Result<(), String> {
    match parse(args)? {
        Command::Help => println!("{HELP}"),
        Command::Emit { frame, raw } => {
            let stdout = io::stdout();
            emit(&mut stdout.lock(), &frame, stdout.is_terminal(), raw)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn cli_uses_protocol_validation_and_complete_frames() {
        for operation in ["begin", "update"] {
            let Command::Emit { frame, raw } = command(&[
                operation,
                "done",
                "--label",
                "Review Δ 🦀",
                "--thread-id",
                "A",
                "--turn-id",
                "T1",
            ])
            .unwrap() else {
                panic!("expected frame")
            };
            assert!(!raw);
            let expected = Snapshot {
                state: rustty::vt::agent::State::Done,
                label: Some("Review Δ 🦀".into()),
                thread_id: Some("A".into()),
                turn_id: Some("T1".into()),
            };
            assert_eq!(
                Event::decode(&frame[19..frame.len() - 2]),
                Some(if operation == "begin" {
                    Event::Begin(expected)
                } else {
                    Event::Update(expected)
                })
            );
        }
        for args in [
            vec![],
            vec!["update"],
            vec!["update", "waiting"],
            vec!["update", "done"],
            vec!["end", "--label", "bad"],
            vec!["update", "idle", "--label"],
            vec!["update", "idle", "--label", "\x1b]"],
            vec!["update", "idle", "--thread-id", "Δ"],
            vec!["begin", "idle", "--label", "one", "--label", "two"],
            vec!["end", "--raw", "--raw"],
            vec!["end", "--pane-id", "A"],
        ] {
            assert!(command(&args).is_err(), "{args:?}");
        }
        assert!(matches!(command(&["--help"]).unwrap(), Command::Help));
    }

    #[test]
    fn captured_output_is_opt_in_and_write_errors_propagate() {
        let Command::Emit { frame, raw } = command(&["end", "--raw"]).unwrap() else {
            panic!("expected frame")
        };
        let mut output = Vec::new();
        assert!(emit(&mut output, &frame, false, false).is_err());
        assert!(output.is_empty());
        emit(&mut output, &frame, false, raw).unwrap();
        assert_eq!(output, Event::End.frame().unwrap());
        emit(&mut output, &frame, true, false).unwrap();
        assert_eq!(output.len(), frame.len() * 2);
        assert!(emit(&mut [0_u8; 1].as_mut_slice(), &frame, true, false).is_err());
    }
}
