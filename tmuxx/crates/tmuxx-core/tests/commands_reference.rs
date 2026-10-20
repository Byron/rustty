mod common;
use common::Pair;

#[test]
#[ignore = "requires a C tmux oracle and socket access"]
fn registry_help_aliases_prefixes_and_argument_errors_match_c() {
    let mut pair = Pair::new();
    for args in [
        vec!["display", "-p", "#{command}"],
        vec!["list-commands"],
        vec!["list-commands", "-h"],
        vec!["list-commands", "-hh"],
        vec!["list-commands", "-h", "new-s"],
        vec!["lscm", "new"],
        vec![
            "list-comm",
            "-F",
            "#{command_list_name}:#{command_list_alias}:#{command_list_name_width}",
        ],
        vec![
            "list-commands",
            "-F",
            "#{?#{==:#{command_list_alias},new},#{command_list_description},}",
        ],
        vec!["list-commands", "-F", ""],
        vec!["list-commands", "list-c"],
        vec!["list-commands", "invalid"],
        vec!["list-c"],
        vec!["list-sessions", "extra"],
        vec!["rename-session"],
        vec!["list-sessions", "-!"],
        vec!["list-sessions", "-z"],
        vec!["list-sessions", "-F"],
        vec!["list-sessions", "-?"],
        vec!["has-s", "-t", "main"],
    ] {
        pair.run(&args);
    }
}

#[test]
#[ignore = "requires a C tmux oracle and socket access"]
fn display_json_canonical_bytes_errors_and_flag_precedence_match_c() {
    let mut pair = Pair::new();
    for input in [
        r#" { "z":true,"a":-9223372036854775808,"m":[{}, {"x":false}] } "#,
        r#"{"esc":"\"\\\/\b\f\n\r\t\u0041"}"#,
        "",
        " ",
        "[]",
        "true",
        "1",
        "\"string\"",
        "{}{}",
        "{",
        r#"{x:1}"#,
        r#"{"":1}"#,
        r#"{"x" 1}"#,
        r#"{"x":}"#,
        r#"{"x":1"#,
        r#"{"x":1 "y":2}"#,
        r#"{"x":1,}"#,
        r#"{"x":1,"x":2}"#,
        r#"{"x":[}"#,
        r#"{"x":[{},]}"#,
        r#"{"x":[{}{}]}"#,
        r#"{"x":""}"#,
        r#"{"x":null}"#,
        r#"{"x":1.0}"#,
        r#"{"x":+1}"#,
        r#"{"x":0x10}"#,
        r#"{"x":9223372036854775808}"#,
        r#"{"x":"bad\q"}"#,
    ] {
        pair.run(&["display-message", "-plj", input]);
    }
    pair.run(&["display-message", "-palj", "{}"]);
    pair.run(&["display-message", "-pIlj", "{}"]);
    pair.run(&["display-message", "-pj"]);
    pair.run(&["display-message", "-p", "-F", "format", "argument"]);
}
