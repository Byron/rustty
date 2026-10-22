use super::Cli;
use clap::{Arg, ArgAction, Command, CommandFactory, Parser, ValueHint};
use clap_complete::Shell;
use std::ffi::OsString;
use std::io::{self, Write};
use tmuxx_command::registry::{COMMANDS, CommandSpec};

#[derive(Parser)]
#[command(
    name = "completions",
    bin_name = "tmuxx completions",
    about = "Print a shell completion script"
)]
struct Completions {
    #[arg(value_enum)]
    shell: Shell,
}

pub(super) fn run(args: &[OsString]) -> io::Result<()> {
    let options = Completions::parse_from(args);
    let mut command = command();
    let mut script = Vec::new();
    clap_complete::generate(options.shell, &mut command, "tmuxx", &mut script);
    if options.shell == Shell::Fish {
        script.extend_from_slice(include_bytes!("completions.fish"));
        // The fish generator handles named options, but not positional shell choices.
        writeln!(
            script,
            "complete -c tmuxx -n '__fish_tmuxx_using_subcommand completions' -f -a 'bash elvish fish powershell zsh'"
        )?;
        for spec in COMMANDS {
            for flag in spec.flags.chars().filter(|flag| *flag != ':') {
                let Some(kind) = target_kind(value_name(spec, flag)) else {
                    continue;
                };
                writeln!(
                    script,
                    "complete -c tmuxx -n '__fish_tmuxx_using_subcommand {} {}' -s {flag} -r -f -a '(__fish_tmuxx_targets {kind})'",
                    spec.name, spec.alias
                )?;
            }
        }
    }
    io::stdout().lock().write_all(&script)
}

// Keep the runtime's trailing argv parser intact while completing its registered commands.
fn command() -> Command {
    let mut command = Command::new("tmuxx")
        .about("Headless tmux-compatible command/control server")
        .disable_help_subcommand(true)
        .args(
            Cli::command()
                .get_arguments()
                .filter(|arg| arg.get_id() != "command")
                .cloned(),
        )
        .subcommand(Completions::command().name("completions"));
    for spec in COMMANDS {
        let mut subcommand = Command::new(spec.name)
            .about(spec.description)
            .disable_help_flag(true);
        if !spec.alias.is_empty() {
            subcommand = subcommand.visible_alias(spec.alias);
        }
        let mut seen = std::collections::BTreeSet::new();
        for (index, flag) in spec.flags.char_indices() {
            if flag == ':' || !seen.insert(flag) {
                continue;
            }
            let mut arg = Arg::new(&spec.flags[index..index + 1]).short(flag);
            if spec.flags.as_bytes().get(index + 1) == Some(&b':') {
                let name = value_name(spec, flag);
                arg = arg.value_name(name).value_hint(match name {
                    "path" | "file" => ValueHint::AnyPath,
                    "directory" | "working-directory" | "start-directory" => ValueHint::DirPath,
                    _ => ValueHint::Other,
                });
            } else {
                arg = arg.action(ArgAction::SetTrue);
            }
            subcommand = subcommand.arg(arg);
        }
        if spec.maximum != Some(0) {
            subcommand = subcommand.arg(Arg::new("arguments").num_args(0..));
        }
        command = command.subcommand(subcommand);
    }
    command
}

fn value_name(spec: &CommandSpec, flag: char) -> &'static str {
    spec.usage
        .split_once(&format!("-{flag} "))
        .and_then(|(_, tail)| tail.split([' ', ']']).next())
        .unwrap_or("value")
}

fn target_kind(name: &str) -> Option<&'static str> {
    match name {
        "target-session" | "src-session" | "dst-session" => Some("session"),
        "target-window" | "src-window" | "source-window" | "dst-window" => Some("window"),
        "target-pane" | "src-pane" | "dst-pane" => Some("pane"),
        "target-client" => Some("client"),
        "buffer-name" => Some("buffer"),
        _ => None,
    }
}
