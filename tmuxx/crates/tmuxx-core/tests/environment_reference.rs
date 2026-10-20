mod common;
use common::Pair;

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn environment_scopes_hidden_cleared_errors_and_format_values_match_c() {
    let mut pair = Pair::new();
    for args in [
        vec!["setenv", "PARITY_LOCAL", "value"],
        vec!["showenv", "PARITY_LOCAL"],
        vec!["setenv", "-g", "PARITY_SHADOW", "global"],
        vec!["display", "-p", "#{PARITY_SHADOW}"],
        vec!["setenv", "-r", "PARITY_SHADOW"],
        vec!["display", "-p", "#{PARITY_SHADOW}"],
        vec!["setenv", "-u", "PARITY_SHADOW"],
        vec!["display", "-p", "#{PARITY_SHADOW}"],
        vec!["setenv", "PARITY_SHADOW", "local"],
        vec!["setenv", "-ur", "PARITY_SHADOW"],
        vec!["showenv", "PARITY_SHADOW"],
        vec!["showenv", "-g", "PARITY_LOCAL"],
        vec!["setenv", "-g", "PARITY_GLOBAL", "global"],
        vec!["showenv", "-g", "PARITY_GLOBAL"],
        vec!["showenv", "PARITY_GLOBAL"],
        vec!["setenv", "-h", "PARITY_LOCAL", "hidden"],
        vec!["showenv", "PARITY_LOCAL"],
        vec!["showenv", "-h", "PARITY_LOCAL"],
        vec!["setenv", "PARITY_LOCAL", "visible"],
        vec!["showenv", "-h", "PARITY_LOCAL"],
        vec!["setenv", "-r", "PARITY_LOCAL"],
        vec!["showenv", "PARITY_LOCAL"],
        vec!["showenv", "-s", "PARITY_LOCAL"],
        vec!["setenv", "-u", "PARITY_LOCAL"],
        vec!["showenv", "PARITY_LOCAL"],
        vec![
            "setenv",
            "-F",
            "PARITY_LOCAL",
            "#{session_name}:#{window_id}",
        ],
        vec!["showenv", "PARITY_LOCAL"],
        vec![
            "setenv",
            "-gF",
            "PARITY_GLOBAL",
            "#{session_name}:#{window_id}",
        ],
        vec!["showenv", "-g", "PARITY_GLOBAL"],
        vec!["setenv", "", "value"],
        vec!["setenv", "A=B", "value"],
        vec!["setenv", "-u", "PARITY_LOCAL", "value"],
        vec!["setenv", "-r", "PARITY_LOCAL", "value"],
        vec!["setenv", "PARITY_LOCAL"],
        vec!["setenv", "-t", "missing", "PARITY_LOCAL", "value"],
        vec!["showenv", "-t", "missing", "PARITY_LOCAL"],
        vec!["showenv", "PARITY_LOCAL"],
    ] {
        pair.run(&args);
    }
    for value in ["", "a$b`c\"d\\e", "line\nnext\tcolumn", "é界"] {
        pair.run(&["setenv", "PARITY_ESC", value]);
        pair.run(&["showenv", "-s", "PARITY_ESC"]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn new_session_environment_overrides_are_ordered_and_local() {
    let mut pair = Pair::new();
    for args in [
        vec!["setenv", "-g", "PARITY_PARENT", "global"],
        vec![
            "new",
            "-ds",
            "child",
            "-e",
            "PARITY_OVERRIDE=first",
            "-e",
            "PARITY_OVERRIDE=last",
            "-e",
            "PARITY_EMPTY=",
            "-e",
            "PARITY_PARENT",
            "sleep 120",
        ],
        vec!["showenv", "-t", "child", "PARITY_OVERRIDE"],
        vec!["showenv", "-t", "child", "PARITY_EMPTY"],
        vec!["showenv", "-t", "child", "PARITY_PARENT"],
        vec!["showenv", "-g", "PARITY_PARENT"],
        vec!["showenv", "-t", "main", "PARITY_OVERRIDE"],
    ] {
        pair.run(&args);
    }
}
