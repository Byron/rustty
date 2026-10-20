//! Exact command observations, with C tmux kept in a separate process.
mod common;
use common::Pair;

#[test]
#[ignore = "requires a C tmux oracle and PTY/socket access"]
fn scope_array_and_display_contracts() {
    let mut pair = Pair::new();
    for args in [
        vec!["set", "-g", "@u", "session-global"],
        vec!["set", "-gw", "@u", "window-global"],
        vec!["set", "-s", "@u", "server"],
        vec!["show", "-gv", "@u"],
        vec!["show", "-gwv", "@u"],
        vec!["show", "-sv", "@u"],
        vec!["set", "-p", "@u", "pane"],
        vec!["show", "-pv", "@u"],
        vec!["set", "-pu", "@u"],
        vec!["show", "-Apv", "@u"],
        vec!["set", "-g", "update-environment", "AAA BBB,CCC"],
        vec!["show", "-g", "update-environment"],
        vec!["set", "-gu", "update-environment[1]"],
        vec!["set", "-ga", "update-environment", "DDD EEE"],
        vec!["show", "-g", "update-environment"],
        vec!["set", "-g", "update-environment[01]", "ONE"],
        vec!["show", "-gv", "update-environment[1]"],
        vec!["show", "-gv", "update-environment[9]"],
        vec!["set", "-go", "update-environment[1]", "changed"],
        vec!["show", "-g", "update-environment"],
        vec!["set", "update-environment[2]", "LOCAL"],
        vec!["show", "-A", "update-environment"],
        vec!["set", "-u", "update-environment"],
        vec!["show", "-A", "update-environment"],
        vec!["set", "-g", "update-environment", ""],
        vec!["show", "-g", "update-environment"],
        vec!["show", "-gv", "update-environment"],
        vec!["show", "-A", "update-environment"],
    ] {
        pair.run(&args);
    }
}

#[test]
#[ignore = "requires a C tmux oracle and PTY/socket access"]
fn scalar_validation_and_quoting_contracts() {
    let mut pair = Pair::new();
    for args in [
        vec!["set", "-g", "history-l", "17"],
        vec!["show", "-gv", "history-limit"],
        vec!["set", "-g", "history-limit", "-1"],
        vec!["set", "-g", "history-limit", "abc"],
        vec!["set", "-g", "history-limit"],
        vec!["set", "-g", "history-limit", "2147483648"],
        vec!["set", "-g", "@empty"],
        vec!["set", "-g", "@words", "two words"],
        vec!["show", "-g", "@words"],
        vec!["set", "-ga", "@words", " again"],
        vec!["show", "-g", "@words"],
        vec!["set", "-go", "@words", "overwrite"],
        vec!["show", "-gv", "@words"],
        vec!["set", "-g", "prefix", "C-a"],
        vec!["show", "-g", "prefix"],
        vec!["set", "-g", "prefix", "boguskey"],
        vec!["set", "-w", "synchronize-panes", "off"],
        vec!["set", "-w", "synchronize-panes"],
        vec!["show", "-wv", "synchronize-panes"],
        vec!["set", "-w", "synchronize-panes"],
        vec!["show", "-wv", "synchronize-panes"],
        vec!["set", "-w", "synchronize-panes", "NO"],
        vec!["set", "-w", "synchronize-panes", "maybe"],
    ] {
        pair.run(&args);
    }
    for value in [
        "",
        "hello",
        "#",
        "~",
        "two words",
        "~user",
        "quote\"",
        "a'\"b",
        "a\\b",
        "a\nb",
        "a\tb",
        "$HOME",
        "a$",
        "a$1",
        "a$?",
        "a$)",
        "a$_X",
        "a${X}",
        "a`b c",
        "#{session_name}",
    ] {
        pair.run(&["set", "-g", "@display", value]);
        pair.run(&["show", "-g", "@display"]);
    }
}

#[test]
#[ignore = "requires a C tmux oracle and PTY/socket access"]
fn head_array_keys_and_output_formats() {
    let mut pair = Pair::new();
    for args in [
        vec!["set", "-g", "update-environment", ""],
        vec!["set", "-g", "update-environment[05]", "five"],
        vec!["set", "-g", "update-environment[zoom]", "zoom"],
        vec!["set", "-g", "update-environment[foo-bar]", "foo-bar"],
        vec!["show", "-g", "update-environment"],
        vec![
            "show",
            "-gF",
            "#{option_name}:#{option_array_key}:#{option_is_array}:#{option_has_value}:#{option_has_array_key}:#{option_value}",
            "update-environment",
        ],
        vec!["set", "-g", "update-environment", ""],
        vec![
            "show",
            "-gvF",
            "#{option_name}:#{option_value_only}:#{option_has_value}:#{option_value}",
            "update-environment",
        ],
        vec!["set", "-g", "@words", "two words"],
        vec![
            "show",
            "-gF",
            "#{option_name}=#{option_value}=#{option_is_user}=#{option_is_string}=#{option_value_only}",
            "@words",
        ],
    ] {
        pair.run(&args);
    }
}

#[test]
#[ignore = "requires the image-enabled C HEAD oracle and PTY/socket access"]
fn all_generated_defaults_and_query_metadata_match_head() {
    let mut pair = Pair::new();
    for scope in ["-gs", "-g", "-gw", "-A", "-Aw", "-Ap"] {
        pair.run(&["show", scope]);
        pair.run(&["show", scope, "-H"]);
        pair.run(&["show", scope, "-H", "-F", "#{option_name}|#{option_value}|#{option_is_parent}|#{option_is_array}|#{option_is_string}|#{option_is_hook}|#{option_has_value}|#{option_array_key}"]);
    }
    for name in [
        "status",
        "status-format",
        "after-new-session",
        "message-style",
        "status-fg",
        "terminal-features",
        "remain-on-exit",
        "prefix2",
    ] {
        pair.run(&["show", "-g", name]);
        pair.run(&[
            "show",
            "-Ag",
            "-F",
            "#{option_name}:#{option_is_string}:#{option_is_hook}:#{option_value}",
            name,
        ]);
    }
    for name in [
        "status-",
        "command-",
        "pane-",
        "prefix",
        "window-size",
        "pane-border-s",
    ] {
        pair.run(&["show", "-g", name]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn default_size_drives_new_windows_with_independent_manual_sizes() {
    let mut pair = Pair::new();
    for args in [
        vec!["set", "-g", "default-size", "101x37"],
        vec!["new", "-d", "-s", "sized", "-n", "initial", "sleep 120"],
        vec!["neww", "-d", "-t", "sized:", "-n", "second", "sleep 120"],
        vec!["resizew", "-t", "sized:0", "-x", "59", "-y", "22"],
        vec!["neww", "-d", "-t", "sized:", "-n", "third", "sleep 120"],
        vec!["set", "-t", "sized", "default-size", "72x19suffix"],
        vec!["neww", "-d", "-t", "sized:", "-n", "suffix", "sleep 120"],
        vec!["set", "-t", "sized", "default-size", "broken"],
        vec!["neww", "-d", "-t", "sized:", "-n", "fallback", "sleep 120"],
        vec!["set", "-g", "default-size", "65x18"],
        vec![
            "new",
            "-d",
            "-s",
            "explicit",
            "-x",
            "88",
            "-n",
            "initial",
            "sleep 120",
        ],
        vec![
            "neww",
            "-d",
            "-t",
            "explicit:",
            "-n",
            "inherited",
            "sleep 120",
        ],
        vec!["new", "-d", "-s", "bad", "-x", "0"],
        vec!["new", "-d", "-s", "bad", "-y", "65536"],
        vec!["new", "-d", "-s", "bad", "-x", "abc"],
    ] {
        pair.run(&args);
        pair.run(&[
            "list-windows",
            "-a",
            "-F",
            "#{session_name}:#{window_index}:#{window_width}x#{window_height}",
        ]);
    }
}

#[test]
#[ignore = "requires C HEAD tmux and PTY/socket access"]
fn window_default_command_precedence_and_start_command_quoting() {
    let mut pair = Pair::new();
    for args in [
        vec!["set", "-g", "default-command", "sleep 120"],
        vec!["set", "-gw", "window-default-command", "exec sleep 120"],
        vec!["neww", "-n", "global"],
        vec!["set", "-w", "window-default-command", "sleep 110"],
        vec!["splitw", "-h"],
        vec!["set", "-w", "window-default-command", ""],
        vec!["splitw", "-v"],
        vec!["set", "-w", "window-default-command", "sleep 100"],
        vec!["respawnp", "-k", "-t", "%2"],
        vec![
            "neww",
            "-n",
            "explicit",
            "/bin/sh",
            "-c",
            "printf '%s' 'a b'; sleep 120",
        ],
    ] {
        pair.run(&args);
        pair.run(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}|#{pane_start_command}|#{pane_start_command_list}",
        ]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn format_option_flags_are_numeric_and_arrays_join_in_key_order() {
    let mut pair = Pair::new();
    for args in [
        vec!["set", "-g", "update-environment", "ALPHA BETA GAMMA"],
        vec!["set", "-g", "update-environment[12]", "LAST"],
        vec!["set", "update-environment[2]", "LOCAL"],
        vec!["set", "-p", "synchronize-panes", "on"],
        vec!["set", "-g", "@user", "on"],
    ] {
        pair.run(&args);
    }
    pair.run(&["display", "-pt", "%0", "#{automatic-rename}|#{synchronize-panes}|#{prefix}|#{remain-on-exit}|#{history-limit}|#{@user}|#{update-environment}|#{update-environment[02]}|#{update-environment[0]}"]);
    pair.run(&["set", "-u", "update-environment"]);
    pair.run(&[
        "display",
        "-pt",
        "%0",
        "#{update-environment}|#{update-environment[12]}|#{update-environment[012]}",
    ]);
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn key_option_aliases_and_modifier_names_are_canonical() {
    let mut pair = Pair::new();
    for value in [
        "c-a", "C-A", "^A", "m-s-Left", "0x7f", "0x41", "0x20", "0x1", "[SOH]", "None", "none",
        "Any", "PageUp", "KP0", "BTab", "S-Tab", "S-BTab", "F13",
    ] {
        pair.run(&["set", "-g", "prefix", value]);
        pair.run(&["show", "-g", "prefix"]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn native_color_style_queries_and_append_match_c() {
    let mut pair = Pair::new();
    for args in [
        vec!["set", "-g", "window-style", "fg=red"],
        vec!["set", "-ag", "window-style", "bg=#123456"],
        vec!["show", "-g", "window-style"],
        vec!["set", "-p", "window-active-style", "FG=90 BG=gray5"],
        vec!["show", "-p", "window-active-style"],
        vec!["set", "-pu", "window-active-style"],
        vec!["show", "-pA", "window-active-style"],
        vec!["set", "-p", "window-style", "bg=nonesuch"],
        vec!["set", "-a", "window-style", "fg=green"],
        vec!["show", "window-style"],
        vec!["set", "-pa", "window-style", "bg=blue"],
        vec!["show", "-p", "window-style"],
        vec!["set", "-pa", "window-style", "fg=yellow"],
        vec!["show", "-p", "window-style"],
    ] {
        pair.run(&args);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn option_name_value_and_output_formats_use_explicit_target_context() {
    let mut pair = Pair::new();
    pair.run(&["neww", "-d", "-t", "main:3", "-n", "other"]);
    pair.run(&["linkw", "-d", "-s", "main:3", "-t", "main:7"]);
    for args in [
        vec![
            "set",
            "-gF",
            "-t",
            "main:7",
            "@#{window_name}",
            "#{session_name}:#{window_index}:#{pane_id}",
        ],
        vec!["show", "-g", "@other"],
        vec![
            "show",
            "-gF",
            "#{session_name}:#{window_index}:#{pane_id}:#{option_name}=#{option_value}",
            "-t",
            "main:7",
            "@#{window_name}",
        ],
        vec![
            "show",
            "-gF",
            "#{session_name}:#{window_index}:#{pane_id}:#{option_name}=#{option_value}",
            "-t",
            "main:3",
            "@other",
        ],
        vec![
            "set",
            "-wF",
            "-t",
            "main:7",
            "@#{window_name}",
            "#{window_index}",
        ],
        vec![
            "show",
            "-wF",
            "#{session_name}:#{window_index}:#{pane_id}:#{option_name}=#{option_value}",
            "-t",
            "main:7",
            "@other",
        ],
    ] {
        pair.run(&args);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn option_unset_only_append_and_array_key_flag_combinations_match_c() {
    let mut pair = Pair::new();
    pair.run(&["splitw", "-h", "-d"]);
    for flags in ["-u", "-U", "-uo", "-Uo", "-uoq", "-Uoq", "-ua", "-Ua"] {
        for name in ["window-style", "@user", "pane-colours[2]"] {
            let value = if name == "window-style" {
                "fg=red"
            } else if name == "@user" {
                "value"
            } else {
                "red"
            };
            pair.run(&["set", "-w", name, value]);
            pair.run(&["set", "-p", "-t", "%0", name, value]);
            pair.run(&["set", "-p", "-t", "%1", name, value]);
            pair.run(&["set", flags, "-w", name]);
            for pane in ["%0", "%1"] {
                pair.run(&["show", "-Ap", "-t", pane, name]);
            }
        }
    }
    for key in ["0", "01", "alpha", "4294967295", "-1", "+1", "a[b"] {
        let name = format!("update-environment[{key}]");
        for flags in ["-g", "-ga", "-go", "-goq", "-gu", "-gU"] {
            pair.run(&["set", flags, &name, "value"]);
            pair.run(&["show", "-g", "update-environment"]);
        }
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn malformed_array_key_diagnostics_are_a_deterministic_native_difference() {
    let mut pair = Pair::new();
    pair.run(&["set", "-g", "update-environment", "KEEP"]);
    for key in ["4294967296", "999999999999999999999999", "", "a]b"] {
        let name = format!("update-environment[{key}]");
        let invalid = format!("invalid option: {name}\n");
        let ambiguous = format!("ambiguous option: {name}\n");
        for command in ["set", "show"] {
            let mut args = vec![command, "-g", &name];
            if command == "set" {
                args.push("changed");
            }
            pair.diagnostic_difference(&args, &invalid, &[&invalid, &ambiguous]);
            pair.run(&["show", "-g", "update-environment"]);
        }
        pair.run(&["set", "-gq", &name, "changed"]);
        pair.run(&["show", "-g", "update-environment"]);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn option_choice_flag_and_numeric_edge_values_match_c() {
    let mut pair = Pair::new();
    for name in [
        "extended-keys",
        "get-clipboard",
        "cursor-style",
        "window-size",
        "remain-on-exit",
    ] {
        for value in [
            "", "o", "on", "off", "yes", "no", "1", "0", "a", "always", "manual", "largest",
            "smallest", "latest", "invalid",
        ] {
            pair.run(&["set", "-g", name, value]);
            pair.run(&["show", "-g", name]);
            pair.run(&["set", "-g", name]);
            pair.run(&["show", "-g", name]);
        }
    }
    for value in ["", "on", "OFF", "yes", "NO", "1", "0", "true", " on"] {
        pair.run(&["set", "-g", "synchronize-panes", value]);
        pair.run(&["show", "-g", "synchronize-panes"]);
    }
    for value in [
        "+3",
        " 3",
        "\t4",
        "3 ",
        "9223372036854775808",
        "-9223372036854775809",
        "999999999999999999999999",
        "-999999999999999999999999",
        "",
    ] {
        pair.run(&["set", "-g", "history-limit", value]);
        pair.run(&["show", "-g", "history-limit"]);
    }
}

#[test]
#[ignore = "requires a C tmux oracle and PTY/socket access"]
fn color_option_validation_and_expansion_match_c() {
    let mut pair = Pair::new();
    for value in [
        "red",
        "90",
        "color12",
        "gray5",
        "#123456",
        "",
        "default",
        "terminal",
        "#{?pane_active,red,blue}",
        "#{?pane_active,red,nonesuch}",
        "fg=red",
        "nonesuch",
        "none",
        "8",
        "9",
        "#abc",
        "rgb:ff/00/00",
        " red",
        "00",
    ] {
        pair.run(&["set", "-p", "cursor-colour", value]);
        pair.run(&["show", "-pv", "cursor-colour"]);
        pair.run(&["display", "-p", "#{cursor_colour}"]);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn declared_option_scope_and_flag_precedence_match_c() {
    let mut pair = Pair::new();
    for bits in 0..16 {
        let flags = b"gspw"
            .iter()
            .enumerate()
            .filter_map(|(bit, flag)| (bits & (1 << bit) != 0).then_some(*flag as char))
            .collect::<String>();
        for (name, value) in [
            ("buffer-limit", "17"),
            ("history-limit", "23"),
            ("automatic-rename", "off"),
            ("synchronize-panes", "on"),
            ("@scope", "value"),
        ] {
            let flag = format!("-{flags}");
            let mut args = vec!["set"];
            if bits != 0 {
                args.push(&flag);
            }
            args.extend([name, value]);
            pair.run(&args);
            let mut args = vec!["show"];
            if bits != 0 {
                args.push(&flag);
            }
            args.push(name);
            pair.run(&args);
        }
        for (flag, name) in [
            ("-s", "buffer-limit"),
            ("-g", "history-limit"),
            ("-A", "history-limit"),
            ("-gw", "automatic-rename"),
            ("-wA", "automatic-rename"),
            ("-gw", "synchronize-panes"),
            ("-wA", "synchronize-panes"),
            ("-pA", "synchronize-panes"),
            ("-s", "@scope"),
            ("-g", "@scope"),
            ("-A", "@scope"),
            ("-gw", "@scope"),
            ("-wA", "@scope"),
            ("-pA", "@scope"),
        ] {
            pair.run(&["show", flag, name]);
        }
    }
    for args in [
        vec!["set", "-g", "history-limit", "31"],
        vec!["set", "-p", "history-limit", "29"],
        vec!["set", "-p", "-t", "missing", "buffer-limit", "19"],
        vec!["show", "-p", "-t", "missing", "buffer-limit"],
        vec!["set", "-q", "history-limit[1]", "2"],
        vec!["show", "-q", "history-limit[1]"],
    ] {
        pair.run(&args);
    }
}
