mod common;
use common::Pair;
#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn window_placement_replacement_insertion_and_last() {
    let mut p = Pair::new();
    for args in [
        vec!["neww", "-d", "-t", "main:", "-n", "w1"],
        vec!["neww", "-d", "-t", "main:", "-n", "w2"],
        vec!["neww", "-d", "-t", "main:9", "-n", "w9"],
        vec!["neww", "-d", "-t", "main:", "-n", "w3"],
        vec!["neww", "-d", "-t", "main:9", "-n", "dup"],
        vec!["neww", "-dk", "-t", "main:9", "-n", "w9k"],
        vec!["neww", "-da", "-t", "main:1", "-n", "wA"],
        vec!["neww", "-db", "-t", "main:0", "-n", "wB"],
        vec!["selectw", "-t", "main:0"],
        vec!["neww", "-S", "-t", "main:", "-n", "w3"],
        vec!["neww", "-S", "-t", "main:9", "-n", "ignored"],
        vec!["neww", "-Sd", "-t", "main:0", "-n", "ignored"],
        vec!["selectw", "-t", "main:0"],
        vec!["neww", "-S", "-t", "main:+6", "-n", "rel6"],
        vec!["last-window", "-t", "main"],
        vec!["next-window", "-t", "main"],
        vec!["previous-window", "-t", "main"],
        vec!["neww", "-dk", "-t", "main:w9k", "-n", "named-replacement"],
        vec![
            "linkw",
            "-k",
            "-s",
            "main:w2",
            "-t",
            "main:named-replacement",
        ],
        vec![
            "neww",
            "-dk",
            "-t",
            "main:{start}",
            "-n",
            "first-replacement",
        ],
        vec!["selectw", "-t", "main:0"],
        vec!["selectw", "-t", "main:1"],
        vec!["linkw", "-dk", "-s", "main:2", "-t", "main:0"],
        vec!["neww", "-d", "-t", "main:missing-name", "-n", "never"],
        vec!["neww", "-d", "-t", "main:+0", "-n", "never"],
        vec!["neww", "-d", "-t", "main:-0", "-n", "never"],
        vec!["neww", "-d", "-t", "main:+foo", "-n", "never"],
        vec!["neww", "-d", "-t", "main:-foo", "-n", "never"],
        vec!["neww", "-d", "-t", "main:+2147483648", "-n", "never"],
    ] {
        p.run(&args);
        p.run(&[
            "list-windows",
            "-t",
            "main",
            "-F",
            "#{window_index}:#{window_name}:#{window_active}:#{window_last_flag}",
        ]);
    }
}
#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn grouped_sessions_share_windows_and_keep_selections() {
    let mut p = Pair::new();
    for args in [
        vec!["new", "-d", "-s", "grouped", "-t", "main"],
        vec!["neww", "-d", "-t", "grouped:", "-n", "other"],
        vec!["selectw", "-t", "main:0"],
        vec!["selectw", "-t", "grouped:1"],
        vec!["killw", "-t", "grouped:0"],
        vec!["neww", "-d", "-t", "main:", "-n", "replacement"],
    ] {
        p.run(&args);
        p.run(&["list-sessions", "-F", "#{session_id}:#{session_name}:#{session_grouped}:#{session_group}:#{session_group_size}:#{session_group_list}"]);
        for session in ["main", "grouped"] {
            p.run(&["list-windows", "-t", session, "-F", "#{window_id}:#{window_index}:#{window_name}:#{window_active}:#{window_linked}:#{window_linked_sessions}"]);
        }
    }
    p.run(&["kill-session", "-t", "grouped"]);
    p.run(&[
        "list-windows",
        "-t",
        "main",
        "-F",
        "#{window_id}:#{window_name}",
    ]);
    p.run(&["display", "-pt", "main", "#{session_group_size}"]);
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn targets_ids_prefix_glob_offsets_and_marked_context() {
    let mut p = Pair::new();
    p.run(&["splitw", "-h", "-t", "main:0"]);
    p.run(&["splitw", "-v", "-t", "%0"]);
    p.run(&["neww", "-dn", "alpha", "-t", "main:3"]);
    p.run(&["neww", "-dn", "alphabet", "-t", "main:8"]);
    for target in [
        "main:0.0",
        "main:0.%1",
        "main:0.+",
        "main:0.-",
        "main:0.{top-left}",
        "main:0.{bottom-right}",
        "main:0.{left-of}",
        "main:0.{right-of}",
        "main:al",
        "main:alphab*",
        "main:=alpha",
        "main:!",
        "main:^",
        "main:$",
        "main:+2",
        "main:-2",
    ] {
        p.run(&[
            "display",
            "-pt",
            target,
            "#{session_id}:#{window_id}:#{pane_id}",
        ]);
    }
    p.run(&["selectp", "-t", "%0"]);
    p.run(&["selectp", "-t", "%1"]);
    p.run(&["last-pane", "-t", "main:0"]);
    p.run(&["display", "-pt", "main:0", "#{pane_id}:#{pane_last}"]);
    p.run(&["selectp", "-m", "-t", "%2"]);
    p.run(&[
        "display",
        "-pt",
        "{marked}",
        "#{session_id}:#{window_id}:#{pane_id}:#{pane_marked}",
    ]);
    p.run(&["new", "-ds", "second"]);
    p.run(&["linkw", "-s", "main:0", "-t", "second:4"]);
    p.run(&[
        "display",
        "-pt",
        "main:0.%2",
        "#{session_name}:#{window_index}:#{pane_id}",
    ]);
    p.run(&[
        "display",
        "-pt",
        "second:4.%2",
        "#{session_name}:#{window_index}:#{pane_id}",
    ]);
    p.run(&["selectp", "-M"]);
    p.run(&["display", "-pt", "{marked}", "#{pane_id}"]);
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn group_name_reuse_does_not_join_unrelated_group() {
    let mut p = Pair::new();
    for args in [
        vec!["new", "-ds", "a"],
        vec!["new", "-ds", "b", "-t", "a"],
        vec!["kill-session", "-t", "a"],
        vec!["new", "-ds", "other", "-t", "a-1"],
        vec!["new", "-ds", "a"],
        vec!["new", "-ds", "c", "-t", "a"],
        vec!["new", "-ds", "d", "-t", "c"],
        vec!["new", "-ds", "e", "-t", "a-1"],
    ] {
        p.run(&args);
        p.run(&[
            "list-sessions",
            "-F",
            "#{session_id}:#{session_name}:#{session_group}:#{session_group_list}:#{window_id}",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux JSON v2 layouts and PTY/socket access"]
fn head_layout_json_has_exact_order_active_and_history_metadata() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "main:0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec!["selectp", "-t", "%1"],
        vec!["selectp", "-t", "%0"],
        vec!["set", "-w", "pane-base-index", "5"],
        vec!["resizep", "-Z", "-t", "%2"],
        vec!["resizep", "-Z", "-t", "%2"],
    ] {
        p.run(&args);
        p.run(&[
            "display",
            "-pt",
            "main:0",
            "#{window_layout}|#{window_visible_layout}",
        ]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn moving_renumbering_and_duplicate_winlinks_preserve_identity() {
    let mut p = Pair::new();
    for args in [
        vec!["neww", "-dn", "one", "-t", "main:1"],
        vec!["neww", "-dn", "two", "-t", "main:2"],
        vec!["neww", "-dn", "three", "-t", "main:3"],
        vec!["movew", "-d", "-s", "main:2", "-t", "main:7"],
        vec!["movew", "-d", "-s", "main:3", "-t", "main:7"],
        vec!["movew", "-dk", "-s", "main:3", "-t", "main:7"],
        vec!["movew", "-da", "-s", "main:7", "-t", "main:0"],
        vec!["movew", "-d", "-s", "main:2", "-t", "main:8"],
        vec!["set", "-t", "main", "base-index", "5"],
        vec!["movew", "-r", "-t", "main"],
        vec!["set", "-t", "main", "base-index", "0"],
        vec!["movew", "-r", "-t", "main"],
        vec!["set", "-t", "main", "renumber-windows", "on"],
        vec!["neww", "-dn", "temporary", "-t", "main:9"],
        vec!["killw", "-t", "main:1"],
        vec!["set", "-t", "main", "renumber-windows", "off"],
        vec!["linkw", "-d", "-s", "main:0", "-t", "main:5"],
        vec!["selectw", "-t", "main:5"],
        vec!["unlinkw", "-t", "main:0"],
        vec!["swapw", "-d", "-s", "main:1", "-t", "main:5"],
    ] {
        p.run(&args);
        p.run(&["list-windows", "-t", "main", "-F", "#{window_id}:#{window_index}:#{window_name}:#{window_active}:#{window_last_flag}:#{window_linked}"]);
        p.run(&[
            "list-panes",
            "-s",
            "-t",
            "main",
            "-F",
            "#{window_index}:#{pane_id}",
        ]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn marked_targets_follow_exact_winlinks_and_invalidate_after_pane_moves() {
    let mut p = Pair::new();
    for args in [
        vec!["linkw", "-d", "-s", "main:0", "-t", "main:4"],
        vec!["selectp", "-m", "-t", "main:4.0"],
        vec!["neww", "-db", "-t", "main:4", "-n", "inserted"],
        vec!["movew", "-r", "-t", "main"],
        vec!["linkw", "-d", "-s", "main:2", "-t", "main:7"],
        vec!["new", "-ds", "second"],
        vec!["linkw", "-d", "-s", "main:7", "-t", "second:3"],
        vec!["selectp", "-m", "-t", "main:0.0"],
        vec!["swapp", "-s", "%0", "-t", "%1"],
    ] {
        p.run(&args);
        p.run(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}:#{window_index}:#{pane_id}:#{pane_marked}:#{pane_marked_set}",
        ]);
        p.run(&[
            "display",
            "-pt",
            "{marked}",
            "#{session_name}:#{window_index}:#{pane_id}",
        ]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn graph_format_fields_keep_winlink_order_indices_and_marks() {
    let mut p = Pair::new();
    for args in [
        vec!["neww", "-dn", "one", "-t", "main:3"],
        vec!["new", "-ds", "zebra"],
        vec!["new", "-ds", "alpha"],
        vec!["linkw", "-d", "-s", "main:0", "-t", "zebra:5"],
        vec!["linkw", "-d", "-s", "main:0", "-t", "alpha:2"],
        vec!["linkw", "-d", "-s", "main:0", "-t", "main:8"],
        vec!["selectw", "-t", "main:8"],
        vec!["selectw", "-t", "zebra:5"],
        vec!["selectp", "-m", "-t", "main:8.0"],
        vec!["neww", "-db", "-t", "main:0", "-n", "inserted"],
        vec!["movew", "-r", "-t", "main"],
        vec!["swapw", "-d", "-s", "main:1", "-t", "alpha:0"],
        vec!["unlinkw", "-k", "-t", "alpha:2"],
        vec!["new", "-ds", "grouped", "-t", "zebra"],
        vec!["neww", "-d", "-t", "zebra:", "-n", "shared"],
        vec!["selectw", "-t", "grouped:5"],
    ] {
        p.run(&args);
        p.run(&["list-sessions", "-F", "#{session_name}|#{active_window_index}|#{last_window_index}|#{next_session_id}|#{server_sessions}|#{session_stack}|#{session_marked}|#{session_format},#{window_format},#{pane_format}"]);
        p.run(&["list-windows", "-a", "-F", "#{session_name}:#{window_index}|#{window_id}|#{window_start_flag}#{window_end_flag}|#{window_stack_index}|#{window_marked_flag}|#{window_flags}|#{window_raw_flags}|#{window_linked_sessions_list}|#{window_active_sessions}:#{window_active_sessions_list}|#{session_format},#{window_format},#{pane_format}"]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn pane_graph_flags_edges_and_unzoomed_geometry_match_c() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "main:0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec!["selectp", "-t", "%1"],
        vec!["resizep", "-Z", "-t", "%1"],
        vec!["resizew", "-x", "93", "-y", "31"],
        vec!["set", "-w", "pane-border-status", "top"],
        vec!["resizep", "-Z", "-t", "%1"],
        vec!["set", "-w", "pane-border-status", "bottom"],
        vec!["set", "-w", "window-size", "manual"],
        vec!["resizew", "-x", "93", "-y", "31"],
    ] {
        p.run(&args);
        p.run(&["list-panes", "-F", "#{pane_id}|#{pane_flags}|#{pane_x},#{pane_y}|#{pane_right},#{pane_bottom}|#{pane_at_top}#{pane_at_bottom}#{pane_at_left}#{pane_at_right}|#{pane_unzoomed_width}x#{pane_unzoomed_height}|#{window_manual_width}x#{window_manual_height}|#{window_panes}|#{session_format},#{window_format},#{pane_format}"]);
        p.run(&[
            "display",
            "-p",
            "#{session_format},#{window_format},#{pane_format}",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux floating pane status and PTY/socket access"]
fn floating_pane_edge_fields_follow_local_border_status_like_c() {
    let mut p = Pair::new();
    p.run(&[
        "newp", "-d", "-t", "%0", "-x", "20", "-y", "7", "-X", "0", "-Y", "1", "",
    ]);
    for status in ["off", "top", "bottom", "top-floating", "bottom-floating"] {
        p.run(&["set", "-w", "pane-border-status", status]);
        for border in ["single", "none"] {
            p.run(&["set", "-p", "-t", "%1", "pane-border-lines", border]);
            for local in ["off", "top", "bottom", "top-floating", "bottom-floating"] {
                p.run(&["set", "-p", "-t", "%1", "pane-border-status", local]);
                for y in ["0", "1", "16", "17"] {
                    p.run(&["movep", "-t", "%1", "-Y", y]);
                    p.run(&["list-panes", "-F", "#{pane_id}|#{pane_x},#{pane_y}|#{pane_width},#{pane_height}|#{pane_at_top}#{pane_at_bottom}#{pane_at_left}#{pane_at_right}|#{pane_unzoomed_width},#{pane_unzoomed_height}"]);
                }
            }
        }
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn alert_flags_belong_to_links_and_survive_grouping_and_renumbering() {
    let mut p = Pair::new();
    p.run(&["neww", "-d", "-t", "main:4", "-n", "alerts", ""]);
    p.run(&["set", "-w", "-t", "main:4", "monitor-activity", "on"]);
    p.run(&["linkw", "-d", "-s", "main:4", "-t", "main:8"]);
    let check = |p: &mut Pair| {
        p.run(&["list-windows", "-aF", "#{session_name}:#{window_index}|#{window_activity_flag}#{window_bell_flag}#{window_silence_flag}|#{window_flags}|#{window_raw_flags}"]);
        p.run(&["list-sessions", "-F", "#{session_name}|#{session_activity_flag}#{session_bell_flag}#{session_silence_flag}|#{session_alert}|#{session_alerts}"]);
    };
    p.feed("%1", 1, b"text\x07\x07", 100);
    check(&mut p);
    for args in [
        vec!["selectw", "-t", "main:4"],
        vec!["new", "-ds", "group", "-t", "main"],
        vec!["movew", "-r", "-t", "main"],
        vec!["selectw", "-t", "group:8"],
        vec!["set", "-w", "-t", "%1", "monitor-bell", "off"],
        vec!["set", "-w", "-t", "%1", "monitor-activity", "off"],
    ] {
        p.run(&args);
        check(&mut p);
    }
    p.feed("%1", 1, b"again\x07", 100);
    check(&mut p);
    p.run(&["set", "-gw", "monitor-activity", "on"]);
    p.run(&[
        "new-window",
        "-d",
        "-t",
        "main:9",
        "-n",
        "initial-activity",
        "sleep 120",
    ]);
    check(&mut p);
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn renumbering_scope_groups_unlink_and_index_wrap_match_c() {
    let mut p = Pair::new();
    for args in [
        vec!["neww", "-dn", "four", "-t", "main:4"],
        vec!["neww", "-dn", "eight", "-t", "main:8"],
        vec!["new", "-ds", "unrelated", "-n", "initial", "sleep 120"],
        vec!["neww", "-dn", "gap", "-t", "unrelated:9"],
        vec!["set", "-t", "unrelated", "renumber-windows", "on"],
        vec!["killw", "-t", "main:4"],
        vec!["new", "-ds", "zgroup", "-t", "main"],
        vec!["set", "-t", "main", "renumber-windows", "on"],
        vec!["set", "-t", "zgroup", "base-index", "5"],
        vec!["neww", "-dn", "twelve", "-t", "main:12"],
        vec!["killw", "-t", "main:8"],
        vec!["neww", "-dn", "ten", "-t", "main:10"],
        vec!["unlinkw", "-k", "-t", "main:0"],
        vec!["neww", "-dn", "eleven", "-t", "main:11"],
        vec!["movew", "-d", "-s", "main:10", "-t", "unrelated:15"],
        vec!["set", "-t", "main", "base-index", "2147483646"],
        vec!["movew", "-r", "-t", "main"],
        vec!["neww", "-dn", "wrapped", "-t", "main:"],
        vec!["neww", "-dn", "wrapped-again", "-t", "main:"],
        vec!["neww", "-da", "-t", "main:2147483647"],
        vec!["neww", "-db", "-t", "main:2147483647"],
        vec!["killw", "-t", "main:2147483647"],
    ] {
        p.run(&args);
        p.run(&["list-windows", "-aF", "#{session_name}:#{window_index}|#{window_id}|#{window_name}|#{window_active}|#{window_stack_index}"]);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn command_names_expand_formats_and_escape_control_characters() {
    let mut p = Pair::new();
    for args in [
        vec!["set", "-g", "@name", "format:.#(ok)"],
        vec!["rename-session", "-t", "$0", ""],
        vec!["rename-session", "-t", "$0", "session:.##(ok)"],
        vec!["rename-session", "-t", "$0", "#{@name}"],
        vec!["rename-window", "-t", "@0", ""],
        vec!["rename-window", "-t", "@0", "window:.##(ok)"],
        vec!["rename-window", "-t", "@0", "#{@name}"],
        vec!["rename-session", "-t", "$0", "main"],
        vec![
            "new-session",
            "-ds",
            "new-session:.##(ok)",
            "-n",
            "new-window:.##(ok)",
            "sleep 120",
        ],
        vec!["new-window", "-dn", "created:.##(ok)", "-t", "$0:"],
        vec!["new-window", "-dn", "#{@name}", "-t", "$0:"],
        vec!["split-window", "-d", "-t", "%0", "sleep 120"],
        vec![
            "break-pane",
            "-d",
            "-s",
            "%4",
            "-t",
            "$0:9",
            "-n",
            "#{@name}\nraw\t\\end",
        ],
        vec![
            "break-pane",
            "-d",
            "-s",
            "%4",
            "-t",
            "$0:10",
            "-n",
            "#{@name}\nmove\t\\end",
        ],
        vec!["rename-session", "-t", "$0", "control\nname\t\\end"],
        vec!["rename-window", "-t", "@0", "control\nname\t\\end"],
    ] {
        p.run(&args);
        p.run(&["list-sessions", "-F", "#{session_id}:#{session_name}"]);
        p.run(&[
            "list-windows",
            "-aF",
            "#{session_id}:#{window_id}:#{window_name}",
        ]);
        p.run(&["list-windows", "-aF", "#{window_id}:#{automatic-rename}"]);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn kill_window_all_filters_duplicates_and_global_renumber_match_c() {
    let mut p = Pair::new();
    let check = |p: &mut Pair| {
        p.run(&["list-windows", "-aF", "#{session_name}:#{window_index}|#{window_id}|#{window_name}|#{window_active}|#{window_stack_index}"])
    };
    for args in [
        vec!["new", "-ds", "unrelated", "-n", "keep", "sleep 120"],
        vec!["neww", "-dn", "gap", "-t", "unrelated:9"],
        vec!["set", "-t", "unrelated", "renumber-windows", "on"],
        vec!["killw", "-a", "-t", "main:0"],
        vec!["killw", "-f", "1", "-t", "main:0"],
        vec!["neww", "-dn", "one", "-t", "main:3"],
        vec!["neww", "-dn", "two", "-t", "main:5"],
        vec!["neww", "-dn", "three", "-t", "main:9"],
        vec!["killw", "-af", "#{==:#{session_windows},4}", "-t", "main:0"],
        vec!["neww", "-dn", "another", "-t", "main:11"],
        vec!["killw", "-af", "#{==:#{window_index},9}", "-t", "main:0"],
        vec!["linkw", "-d", "-s", "main:0", "-t", "main:17"],
        vec!["killw", "-af", "0", "-t", "main:0"],
        vec!["killw", "-af", "#{==:#{window_index},17}", "-t", "main:0"],
        vec!["killw", "-a", "-t", "main:5"],
    ] {
        p.run(&args);
        check(&mut p);
    }
    p.run(&["linkw", "-d", "-s", "main:5", "-t", "main:23"]);
    p.run(&["killw", "-a", "-t", "main:5"]);
    check(&mut p);
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn observable_format_fields_without_graph_context_match_c() {
    let mut p = Pair::new();
    let scope: serde_json::Value =
        serde_json::from_str(include_str!("../../../conformance/scope.json")).unwrap();
    let fields = scope["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["id"] == "format-observable-state")
        .unwrap()["formats"]
        .as_array()
        .unwrap();
    for field in fields.iter().map(|field| field.as_str().unwrap()) {
        if matches!(
            field,
            "host"
                | "host_short"
                | "pid"
                | "uid"
                | "user"
                | "socket_path"
                | "start_time"
                | "version"
        ) {
            continue;
        }
        p.run(&[
            "list-commands",
            "-F",
            &format!("{field}=#{{{field}}}"),
            "new-session",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn pane_input_and_output_generation_fields_match_c() {
    let mut p = Pair::new();
    p.run(&["new-window", "-d", "-t", "main:1", "-n", "empty", ""]);
    let check = |p: &mut Pair| {
        p.run(&[
            "display",
            "-pt",
            "%1",
            "#{pane_input_off}|#{pane_output_generation}",
        ])
    };
    check(&mut p);
    for bytes in [b"hello".as_slice(), b"\x1b[", b"31mred", b"", b"\x07"] {
        p.feed("%1", 1, bytes, bytes.len());
        check(&mut p);
    }
    for flag in ["-d", "-d", "-e"] {
        p.run(&["select-pane", flag, "-t", "%1"]);
        check(&mut p);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn kill_session_clear_alerts_groups_and_filters_match_c() {
    let mut p = Pair::new();
    p.run(&["neww", "-dn", "alerts", "-t", "main:3", ""]);
    p.run(&["set", "-w", "-t", "main:3", "monitor-activity", "on"]);
    p.run(&["new", "-ds", "shared", "-t", "main"]);
    p.feed("%1", 1, b"activity\x07", 100);
    let check = |p: &mut Pair| {
        p.run(&[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_group}|#{session_alerts}",
        ]);
        p.run(&[
            "list-windows",
            "-aF",
            "#{session_name}:#{window_index}|#{window_id}|#{window_flags}",
        ]);
    };
    check(&mut p);
    for args in [
        vec!["kill-session", "-C", "-t", "main"],
        vec!["kill-session", "-aCf", "1", "-t", "main"],
        vec!["kill-session", "-f", "1", "-t", "main"],
        vec!["kill-session", "-aC", "-t", "shared"],
        vec!["new", "-ds", "zebra"],
        vec!["new", "-ds", "alpha"],
        vec!["new", "-ds", "beta"],
        vec![
            "kill-session",
            "-af",
            "#{==:#{server_sessions},5}",
            "-t",
            "main",
        ],
        vec![
            "kill-session",
            "-agf",
            "#{==:#{session_name},beta}",
            "-t",
            "main",
        ],
        vec!["kill-session", "-g", "-t", "shared"],
        vec!["new", "-ds", "another"],
        vec!["kill-session", "-g", "-t", "another"],
    ] {
        p.run(&args);
        check(&mut p);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn window_navigation_toggle_alerts_and_flag_precedence_match_c() {
    let mut p = Pair::new();
    let check = |p: &mut Pair| {
        p.run(&["list-windows", "-aF", "#{session_name}:#{window_index}|#{window_active}|#{window_last_flag}|#{window_stack_index}|#{window_flags}"])
    };
    for args in [
        vec!["next", "-t", "main"],
        vec!["prev", "-t", "main"],
        vec!["last", "-t", "main"],
        vec!["selectw", "-T", "-t", "main:0"],
        vec!["neww", "-dn", "one", "-t", "main:3", ""],
        vec!["neww", "-dn", "two", "-t", "main:7", ""],
        vec!["neww", "-dn", "three", "-t", "main:11", ""],
        vec!["selectw", "-npl", "-t", "main:11"],
        vec!["selectw", "-pl", "-t", "main:0"],
        vec!["selectw", "-l", "-t", "main:11"],
        vec!["selectw", "-T", "-t", "main:3"],
        vec!["selectw", "-T", "-t", "main:11"],
        vec!["next", "-a", "-t", "main"],
        vec!["prev", "-a", "-t", "main"],
    ] {
        p.run(&args);
        check(&mut p);
    }
    p.feed("%1", 1, b"\x07", 1);
    p.feed("%2", 2, b"\x07", 1);
    for args in [
        vec!["next", "-a", "-t", "main"],
        vec!["prev", "-a", "-t", "main"],
        vec!["next", "-a", "-t", "main"],
        vec!["selectw", "-T", "-t", "main:3"],
        vec!["next", "-t", "main:0"],
        vec!["prev", "-t", "main:11"],
    ] {
        p.run(&args);
        check(&mut p);
    }
}
