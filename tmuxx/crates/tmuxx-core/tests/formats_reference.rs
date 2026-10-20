mod common;
use common::Pair;

#[test]
#[ignore = "requires a C tmux oracle and socket access"]
fn pane_content_formats_search_visible_rows_and_application_screen() {
    let mut pair = Pair::new();
    pair.run(&["new-window", "-d", "-n", "search", ""]);
    for bytes in [
        b"old\r\nZebra_Marker_42\r\nthird\tspace\r\n".as_slice(),
        b"\x1b[?1049hALT\r\nZebra_alt\r\n",
        b"\x1b[?1049l",
    ] {
        pair.feed("%1", 1, bytes, 1);
        for format in [
            "#{C:Zebra_Marker_42}",
            "#{C:Absent_String_999}",
            "#{C/r:Zebra_.*}",
            "#{C/i:zebra*}",
            "#{C:}",
            "#{C/r:}",
            "#{C/r:[}",
            "#{C:^old}",
            "#{C:ALT}",
            "#{C/r: +$}",
            "#{C;n:Zebra}",
        ] {
            pair.run(&["display-message", "-p", "-t", "%1", format]);
        }
    }
    pair.run(&["resize-window", "-t", "search", "-x", "12", "-y", "3"]);
    pair.feed(
        "%1",
        1,
        b"\x1b[2J\x1b[Hhistory gone\r\none\r\ntwo\r\nthree\r\nfour",
        1,
    );
    for format in [
        "#{C:history}",
        "#{C:one}",
        "#{C:two}",
        "#{C:three}",
        "#{C:four}",
        "#{C/r:^four$}",
        "#{C/i:THREE}",
    ] {
        pair.run(&["display-message", "-p", "-t", "%1", format]);
    }
    pair.feed("%1", 1, "\x1b[2J\x1b[HÉté 界".as_bytes(), 1);
    for format in [
        "#{C/i:été}",
        "#{C/ri:été}",
        "#{C:É?? ?}",
        "#{C/r:^[[:alpha:]]+ .$}",
    ] {
        pair.run(&["display-message", "-p", "-t", "%1", format]);
    }
}

#[test]
#[ignore = "requires a C tmux oracle and socket access"]
fn contextual_session_window_pane_loops_and_name_checks_match_c() {
    let mut pair = Pair::new();
    for args in [
        vec![
            "new-session",
            "-d",
            "-s",
            "alpha",
            "-n",
            "alpha",
            "sleep 120",
        ],
        vec![
            "new-window",
            "-d",
            "-t",
            "main:2",
            "-n",
            "zeta",
            "sleep 120",
        ],
        vec![
            "new-window",
            "-d",
            "-t",
            "main:1",
            "-n",
            "beta",
            "sleep 120",
        ],
        vec!["split-window", "-d", "-t", "main:0", "sleep 120"],
        vec!["split-window", "-d", "-t", "main:0", "sleep 120"],
        vec!["swap-pane", "-d", "-s", "%0", "-t", "%5"],
        vec!["set-option", "-w", "-t", "main:0", "@neighbour", "first"],
    ] {
        pair.run(&args);
    }
    for format in [
        "#{S:#{session_id}:#{session_name}:#{loop_index}:#{loop_last_flag}|}",
        "#{S/nr:#{session_name}|}",
        "#{W:#{window_index}:#{window_name}:#{loop_index}:#{loop_last_flag}|}",
        "#{W/nr:#{window_name}|}",
        "#{W:(#{window_name}),[#{window_name}]}",
        "#{P:#{pane_id}:#{pane_index}|}",
        "#{P/i:#{pane_id}:#{pane_index}|}",
        "#{P/ir:#{pane_id}|}",
        "#{P:(#{pane_id}),[#{pane_id}]}",
        "#{S:#{session_name}[#{W:#{window_name}[#{P:#{pane_id}|}]}]}",
        "#{W:#{window_index}:#{prev_window_index}:#{next_window_index}:#{window_before_active}:#{window_after_active}:#{prev_@neighbour}|}",
        "#{N/s:alpha}:#{N/s:missing}:#{N:initial}:#{N/w:missing}",
        "#{n;S:#{session_name}}",
        "#{t/f/%%Y-%%m-%%d:#{l:1700000000}}",
    ] {
        pair.run(&["display-message", "-p", "-t", "main:0", format]);
    }
}

#[test]
#[ignore = "requires a current C tmux oracle and socket access"]
fn option_and_environment_scopes_arrays_and_removed_entries_match_c() {
    let mut pair = Pair::new();
    for args in [
        vec!["set-option", "-t", "main", "@alpha", "first"],
        vec!["set-option", "-t", "main", "@zeta", "last"],
        vec!["set-option", "-t", "main", "update-environment", "one two"],
        vec!["set-option", "-t", "main", "update-environment[7]", "third"],
        vec!["set-option", "-w", "-t", "main:0", "@window", "local"],
        vec!["set-option", "-p", "-t", "%0", "@pane", "local"],
        vec!["set-option", "-g", "@global", "global"],
        vec!["set-environment", "-t", "main", "PARITY_A", "value"],
        vec!["set-environment", "-ht", "main", "PARITY_B", "hidden"],
        vec!["set-environment", "-rt", "main", "PARITY_C"],
        vec!["set-environment", "-g", "PARITY_G", "global"],
    ] {
        pair.run(&args);
    }
    for flags in ["", "/s", "/w", "/p", "/gp", "/bad"] {
        let format = format!(
            "#{{O{flags}:#{{option_name}}=#{{option_value}}:#{{option_is_user}}:#{{option_is_array}}:#{{option_is_hook}}:#{{option_array_key}}:#{{option_array_count}}:#{{option_array_first}}:#{{option_array_last}}:#{{loop_index}}:#{{loop_last_flag}}|}}"
        );
        pair.run(&["display-message", "-p", "-t", "main:0", &format]);
    }
    for flags in ["", "/s", "/g", "/wrong"] {
        let format = format!(
            "#{{V{flags}:#{{?#{{m:PARITY_*,#{{environ_name}}}},#{{environ_name}}=#{{environ_value}}:#{{environ_hidden}}:#{{environ_removed}}|,}}}}"
        );
        pair.run(&["display-message", "-p", "-t", "main:0", &format]);
    }
    pair.run(&[
        "display-message",
        "-p",
        "-t",
        "main:0",
        "#{O/g:#{?#{m:@*,#{option_name}},#{option_name}=#{option_value}|,}}",
    ]);
    pair.run(&[
        "display-message",
        "-p",
        "-t",
        "main:0",
        "#{n;V/s:#{environ_name}}",
    ]);
}

#[test]
#[ignore = "requires current C tmux floating-pane support"]
fn pane_z_sort_and_flag_precedence_match_c() {
    let mut pair = Pair::new();
    for args in [
        vec!["split-window", "-d", "-t", "main:0", "sleep 120"],
        vec![
            "new-pane",
            "-df",
            "-t",
            "main:0",
            "-x",
            "12",
            "-y",
            "6",
            "sleep 120",
        ],
        vec![
            "new-pane",
            "-df",
            "-t",
            "main:0",
            "-x",
            "12",
            "-y",
            "6",
            "sleep 120",
        ],
        vec!["select-pane", "-t", "%2"],
    ] {
        pair.run(&args);
    }
    for format in [
        "#{P/z:#{pane_id}:#{pane_index}:#{pane_z}|}",
        "#{P/zr:#{pane_id}:#{pane_z}|}",
        "#{P/iz:#{pane_id}:#{pane_index}|}",
        "#{S/in:#{session_id}:#{session_name}|}",
        "#{W/in:#{window_index}:#{window_name}|}",
        "#{P/unknown:#{pane_id}|}",
    ] {
        pair.run(&["display-message", "-p", "-t", "main:0", format]);
    }
}

#[test]
#[ignore = "requires the image-enabled C HEAD oracle and socket access"]
fn global_option_collections_and_hook_metadata_match_head() {
    let mut pair = Pair::new();
    for flags in ["g", "gs", "gw", "v", "gv", "gvws", "gws", "gwp"] {
        let format = format!(
            "#{{O/{flags}:#{{option_name}}=#{{option_value}}:#{{option_is_user}}:#{{option_is_array}}:#{{option_is_hook}}:#{{option_array_key}}:#{{option_array_count}}:#{{option_array_first}}:#{{option_array_last}}:#{{loop_index}}:#{{loop_last_flag}}\\n}}"
        );
        pair.run(&["display-message", "-p", "-t", "main:0", &format]);
    }
}

#[test]
#[ignore = "requires a C tmux oracle and socket access"]
fn contextual_resource_bounds_are_an_explicit_reference_difference() {
    #[cfg(unix)]
    use std::{
        path::PathBuf,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tmuxx_core::{ClientContext, Engine};
    struct Oracle {
        #[cfg(unix)]
        program: PathBuf,
        #[cfg(unix)]
        socket: PathBuf,
        #[cfg(windows)]
        remote: common::remote_reference::RemoteReference,
    }
    impl Oracle {
        fn run(&self, args: &[&str]) -> Vec<u8> {
            #[cfg(unix)]
            let output = Command::new(&self.program)
                .arg("-S")
                .arg(&self.socket)
                .args(["-f", "/dev/null"])
                .args(args)
                .output()
                .unwrap();
            #[cfg(windows)]
            let output = self.remote.output(
                &args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
                &[],
            );
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        }
    }
    #[cfg(unix)]
    impl Drop for Oracle {
        fn drop(&mut self) {
            let _ = Command::new(&self.program)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .output();
        }
    }
    #[cfg(unix)]
    let oracle = Oracle {
        program: std::env::var_os("TMUXX_C_TMUX")
            .expect("set TMUXX_C_TMUX")
            .into(),
        socket: std::env::temp_dir().join(format!(
            "tx-format-bound-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )),
    };
    #[cfg(windows)]
    let oracle = Oracle {
        remote: common::remote_reference::RemoteReference::new(),
    };
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    let create = ["new-session", "-d", "-s", "main", "sleep 120"];
    oracle.run(&create);
    engine
        .execute(&tmuxx_command::parse_argv(create).unwrap()[0], &mut context)
        .unwrap();
    for count in [16, 17] {
        let format = format!("{}leaf{}", "#{W:".repeat(count), "}".repeat(count));
        assert_eq!(
            oracle.run(&["display-message", "-p", "-t", "main", &format]),
            b"leaf\n"
        );
        let result = engine.expand(format.as_bytes(), Some(0), Some(0), Some(0), &context);
        if count == 16 {
            assert_eq!(result.unwrap(), b"leaf");
        } else {
            assert_eq!(result.unwrap_err(), "format context nesting limit exceeded");
        }
    }
    for index in 1..17 {
        let name = format!("other{index}");
        let create = ["new-session", "-d", "-s", &name, "sleep 120"];
        oracle.run(&create);
        engine
            .execute(&tmuxx_command::parse_argv(create).unwrap()[0], &mut context)
            .unwrap();
    }
    let format = "#{S:#{R:abcdefgh,8192}}";
    let c = oracle.run(&["display-message", "-p", format]);
    assert_eq!(c, [b"abcdefgh".repeat(8192 * 17), b"\n".to_vec()].concat());
    assert_eq!(
        engine
            .expand(format.as_bytes(), None, None, None, &context)
            .unwrap_err(),
        "format loop output exceeds limit"
    );
}

#[test]
#[ignore = "requires current C tmux oracle and socket access"]
fn nested_collections_use_resolved_graph_context_types() {
    let mut pair = Pair::new();
    pair.run(&["set", "@one", "value"]);
    pair.run(&["set", "-w", "@one", "value"]);
    pair.run(&["set", "-p", "@one", "value"]);
    pair.run(&["setenv", "TYPE_CONTEXT", "value"]);
    let flags =
        "#{session_format}#{window_format}#{pane_format}:#{session_id}:#{window_id}:#{pane_id}";
    let format = format!(
        "{flags}|#{{O:#{{?#{{==:#{{option_name}},@one}},{flags},}}}}|#{{O/w:#{{?#{{==:#{{option_name}},@one}},{flags},}}}}|#{{O/p:#{{?#{{==:#{{option_name}},@one}},{flags},}}}}|#{{V:#{{?#{{==:#{{environ_name}},TYPE_CONTEXT}},{flags},}}}}"
    );
    for command in ["list-sessions", "list-windows", "list-panes"] {
        pair.run(&[command, "-F", &format]);
    }
    pair.run(&["display-message", "-p", &format]);
    pair.run(&["list-commands", "-F", &format, "new-session"]);
    pair.run(&[
        "display-message",
        "-p",
        &format!("#{{S:{format}|#{{W:{format}|#{{P:{format}}}}}}}"),
    ]);
}
