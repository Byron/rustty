mod common;
use common::Pair;

fn state(p: &mut Pair) {
    p.run(&["list-panes", "-a", "-F", "#{session_name}:#{window_index}:#{window_id}:#{pane_index}:#{pane_id}:#{pane_active}:#{pane_last}:#{pane_left},#{pane_top},#{pane_width},#{pane_height}"]);
    p.run(&["list-windows", "-a", "-F", "#{session_name}:#{window_index}:#{window_id}:#{window_name}:#{window_active}:#{window_zoomed_flag}"]);
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn rotate_and_swap_preserve_positions_and_selection() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-l", "24", "-t", "%0"],
        vec!["splitw", "-v", "-l", "7", "-t", "%0"],
        vec!["rotatew", "-D", "-t", "main:0"],
        vec!["rotatew", "-U", "-t", "main:0"],
        vec!["swapp", "-s", "%0", "-t", "%1"],
        vec!["swapp", "-d", "-s", "%1", "-t", "%2"],
        vec!["swapp", "-D", "-t", "%0"],
        vec!["swapp", "-U", "-t", "%0"],
        vec!["resizep", "-Z", "-t", "%2"],
        vec!["rotatew", "-DZ", "-t", "main:0"],
        vec!["swapp", "-Z", "-s", "%0", "-t", "%1"],
        vec!["neww", "-dn", "other", "-t", "main:1"],
        vec!["swapp", "-s", "%0", "-t", "%3"],
        vec!["swapp", "-d", "-s", "%1", "-t", "%0"],
    ] {
        p.run(&args);
        state(&mut p);
    }
}

#[test]
#[ignore = "requires C tmux and PTY/socket access"]
fn join_and_break_preserve_pane_identity_and_nested_geometry() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "%0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec!["neww", "-dn", "other", "-t", "main:1"],
        vec!["joinp", "-dh", "-l", "15", "-s", "%2", "-t", "%3"],
        vec!["joinp", "-vb", "-l", "7", "-s", "%0", "-t", "%2"],
        vec!["movep", "-h", "-l", "10", "-s", "%3", "-t", "%0"],
        vec!["breakp", "-dP", "-n", "broken", "-s", "%0", "-t", "main:7"],
        vec!["breakp", "-P", "-n", "moved", "-s", "%1", "-t", "main:8"],
        vec!["joinp", "-s", "%1", "-t", "%0"],
        vec![
            "breakp", "-bP", "-n", "inserted", "-s", "%1", "-t", "main:1",
        ],
        vec!["joinp", "-s", "%2", "-t", "%2"],
        vec!["joinp", "-h", "-s", "%1", "-t", "main:404"],
    ] {
        p.run(&args);
        state(&mut p);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn full_window_splits_and_joins_use_proportional_geometry() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-l", "20", "-t", "%0"],
        vec!["splitw", "-v", "-l", "8", "-t", "%0"],
        vec!["splitw", "-fh", "-l", "15", "-t", "%2"],
        vec!["splitw", "-fvb", "-l", "3", "-t", "%0"],
        vec!["joinp", "-fh", "-l", "10", "-s", "%4", "-t", "%0"],
        vec!["joinp", "-dh", "-l", "0", "-s", "%4", "-t", "%1"],
        vec!["selectp", "-m", "-t", "%4"],
        vec!["movep", "-dbv", "-l", "4", "-s", "%4", "-t", "%0"],
        vec!["breakp", "-d", "-n", "broken", "-s", "%4", "-t", "main:4"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}:#{pane_marked}:#{pane_marked_set}",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn preset_layouts_cycles_spread_and_restore() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "%0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec!["splitw", "-h", "-t", "%2"],
        vec!["splitw", "-v", "-t", "%1"],
        vec!["selectl", "-t", "main:0"],
        vec!["selectl", "-E", "-t", "%2"],
        vec!["selectl", "-o", "-t", "main:0"],
        vec!["nextl", "-t", "main:0"],
        vec!["nextl", "-t", "main:0"],
        vec!["nextl", "-t", "main:0"],
        vec!["nextl", "-t", "main:0"],
        vec!["nextl", "-t", "main:0"],
        vec!["nextl", "-t", "main:0"],
        vec!["nextl", "-t", "main:0"],
        vec!["selectl", "-o", "-t", "main:0"],
        vec!["selectl", "-p", "-t", "main:0"],
        vec!["set", "-w", "main-pane-width", "60%"],
        vec!["set", "-w", "other-pane-height", "7"],
        vec!["selectl", "-t", "main:0", "main-vertical"],
        vec!["selectl", "-t", "main:0", "main-horizontal"],
        vec!["selectl", "-t", "main:0", "main-vertical-mirrored"],
        vec!["set", "-w", "main-pane-height", "40%"],
        vec!["selectl", "-t", "main:0", "main-horizontal-mirrored"],
        vec!["set", "-w", "tiled-layout-max-columns", "1"],
        vec!["selectl", "-t", "main:0", "tiled"],
        vec!["selectl", "-t", "main:0", "even-h"],
        vec!["resizep", "-L", "5", "-t", "%3"],
        vec!["selectl", "-t", "main:0"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    }
}

#[test]
#[ignore = "requires current C tmux JSON layouts and PTY/socket access"]
fn custom_layout_indexes_active_history_and_excess_cells() {
    let mut p = Pair::new();
    p.run(&["splitw", "-h", "-t", "%0"]);
    p.run(&["splitw", "-v", "-t", "%0"]);
    let layout = r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":20,"h":24,"x":0,"y":0,"i":12,"a":true,"I":"%400"},{"t":"p","w":30,"h":24,"x":21,"y":0,"i":10,"l":5,"I":"%400"},{"t":"p","w":28,"h":24,"x":52,"y":0,"i":11,"l":2,"I":"%400"}]}}"#;
    p.run(&["selectl", "-t", "main:0", layout]);
    state(&mut p);
    p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    p.run(&["selectl", "-t", "main:0", "main-vertical-mirrored"]);
    p.run(&["selectl", "-o", "-t", "main:0"]);
    state(&mut p);
    p.run(&["killp", "-t", "%1"]);
    p.run(&["selectl", "-t", "main:0", layout]);
    state(&mut p);
    p.run(&["selectl", "-t", "main:0", "even-vertical"]);
    p.run(&["selectl", "-o", "-t", "main:0"]);
    state(&mut p);
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn floating_panes_preserve_tree_positions_stacking_and_retile() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "%0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec![
            "breakp", "-W", "-s", "%2", "-x", "24", "-y", "8", "-X", "-3", "-Y", "2",
        ],
        vec!["breakp", "-Wd", "-s", "%1"],
        vec!["joinp", "-s", "%2", "-t", "%2"],
        vec!["breakp", "-W", "-s", "%2"],
        vec!["joinp", "-s", "%1", "-t", "%1"],
        vec!["joinp", "-s", "%2", "-t", "%2"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
        p.run(&[
            "list-panes",
            "-t",
            "main:0",
            "-F",
            "#{pane_id}:#{pane_floating_flag}:#{pane_z}",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn all_floating_windows_resize_and_presets_keep_independent_geometry() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "%0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec!["breakp", "-W", "-s", "%0"],
        vec!["breakp", "-W", "-s", "%2"],
        vec!["breakp", "-W", "-s", "%1"],
        vec!["resizew", "-t", "main:0", "-x", "100", "-y", "40"],
        vec!["selectl", "-t", "main:0", "tiled"],
        vec!["joinp", "-s", "%0", "-t", "%0"],
        vec!["splitw", "-h", "-t", "%0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec!["selectl", "-t", "main:0", "even-horizontal"],
        vec!["selectl", "-t", "main:0", "main-vertical"],
        vec!["selectl", "-t", "main:0", "tiled"],
        vec!["killp", "-t", "%2"],
        vec!["joinp", "-s", "%1", "-t", "%1"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    }
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn floating_move_resize_positions_and_z_order() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "%0"],
        vec!["splitw", "-v", "-t", "%0"],
        vec!["breakp", "-W", "-s", "%2"],
        vec!["breakp", "-W", "-s", "%1"],
        vec!["movep", "-t", "%2", "-X", "-5", "-Y", "50%"],
        vec!["movep", "-t", "%2", "-U", "2", "-R", "4"],
        vec!["movep", "-t", "%2", "-P", "front"],
        vec!["movep", "-t", "%1", "-z", "0"],
        vec!["movep", "-t", "%1", "-P", "backward-loop"],
        vec!["movep", "-t", "%2", "-P", "bottom-right"],
        vec!["resizep", "-t", "%2", "-x", "30", "-y", "10"],
        vec!["resizep", "-t", "%2", "-U", "2", "-L", "4"],
        vec!["resizep", "-t", "%2", "-R", "-3", "-D", "1"],
        vec!["set", "-p", "-t", "%2", "pane-border-lines", "none"],
        vec!["movep", "-t", "%2", "-P", "top-left"],
        vec!["resizep", "-t", "%2", "-x", "30", "-y", "10"],
        vec!["movep", "-t", "%0", "-P", "center"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    }
}

#[test]
#[ignore = "requires current C tmux new-pane/modal semantics and PTY/socket access"]
fn new_floating_and_modal_panes_share_process_graph_and_selection() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-h", "-t", "%0"],
        vec![
            "newp",
            "-dP",
            "-t",
            "%0",
            "-x",
            "24",
            "-y",
            "8",
            "sleep 120",
        ],
        vec![
            "newp",
            "-OP",
            "-t",
            "%0",
            "-x",
            "20",
            "-y",
            "10",
            "sleep 120",
        ],
        vec!["selectp", "-t", "%0"],
        vec!["newp", "-O", "-t", "%0", "sleep 120"],
        vec!["breakp", "-s", "%3", "-t", "main:2"],
        vec!["joinp", "-s", "%3", "-t", "%3"],
        vec!["swapp", "-s", "%3", "-t", "%0"],
        vec!["killp", "-t", "%3"],
        vec!["selectp", "-t", "%0"],
        vec![
            "newp",
            "-b",
            "-B",
            "none",
            "-t",
            "%0",
            "-x",
            "20",
            "-y",
            "10",
            "sleep 120",
        ],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&[
            "display",
            "-pt",
            "main:0",
            "#{window_layout}:#{window_modal_pane}",
        ]);
        p.run(&[
            "list-panes",
            "-t",
            "main:0",
            "-F",
            "#{pane_id}:#{pane_modal_flag}",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn floating_restore_geometry_and_modal_stacking() {
    let mut p = Pair::new();
    for args in [
        vec!["newp", "-t", "%0", "sleep 120"],
        vec!["movep", "-t", "%1", "-X", "-4", "-Y", "7"],
        vec!["resizep", "-t", "%1", "-x", "28", "-y", "11"],
        vec!["joinp", "-s", "%1", "-t", "%1"],
        vec!["breakp", "-W", "-s", "%1"],
        vec!["newp", "-O", "-t", "%0", "sleep 120"],
        vec!["newp", "-d", "-t", "%0", "sleep 120"],
        vec!["newp", "-t", "%0", "sleep 120"],
        vec!["killp", "-t", "%2"],
        vec!["selectp", "-t", "%3"],
        vec!["selectp", "-t", "%1"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&[
            "display",
            "-pt",
            "main:0",
            "#{window_layout}:#{window_modal_pane}",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn legacy_layout_keeps_floats_and_swaps_transfer_cell_geometry() {
    let mut p = Pair::new();
    let layout = tmuxx_layout::Layout::pane(10, 100, 30).to_legacy().unwrap();
    for args in [
        vec!["newp", "-t", "%0", "sleep 120"],
        vec!["selectl", "-t", "main:0", &layout],
        vec!["swapp", "-d", "-s", "%0", "-t", "%1"],
        vec!["swapp", "-s", "%0", "-t", "%1"],
        vec!["swapp", "-D", "-t", "%1"],
        vec!["swapp", "-U", "-t", "%1"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    }
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn creation_tiled_and_floating_split_flags() {
    let mut p = Pair::new();
    for args in [
        vec!["newp", "-Lh", "-l", "25", "-t", "%0", "sleep 120"],
        vec!["newp", "-Lbvf", "-l", "4", "-t", "%1", "sleep 120"],
        vec!["newp", "-t", "%0", "-x", "24", "-y", "9", "sleep 120"],
        vec!["splitw", "-h", "-t", "%3", "sleep 120"],
        vec!["splitw", "-bv", "-t", "%4", "sleep 120"],
        vec!["splitw", "-fh", "-t", "%5", "sleep 120"],
        vec!["newp", "-L", "-t", "%3", "sleep 120"],
        vec!["newp", "-LO", "-t", "%0", "sleep 120"],
        vec!["newp", "-bdf", "-t", "%0", "sleep 120"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    }
}

#[test]
#[ignore = "requires current C tmux floating zoom and PTY/socket access"]
fn zoomed_creation_and_overlay_geometry_survive_unzoom() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-dh", "-t", "%0"],
        vec![
            "newp",
            "-Z",
            "-t",
            "%0",
            "-x",
            "40",
            "-y",
            "12",
            "sleep 120",
        ],
        vec!["resizep", "-Z", "-t", "%2"],
        vec!["killp", "-t", "%2"],
        vec!["resizep", "-Z", "-t", "%0"],
        vec!["newp", "-A", "-t", "%0", "-x", "20", "-y", "8", "sleep 120"],
        vec!["movep", "-t", "%3", "-P", "bottom-right"],
        vec!["resizep", "-t", "%3", "-x", "30", "-y", "10"],
        vec!["resizep", "-Z", "-t", "%0"],
        vec!["resizep", "-Z", "-t", "%0"],
        vec!["newp", "-O", "-t", "%0", "sleep 120"],
        vec!["killp", "-t", "%4"],
        vec!["selectp", "-t", "%1"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}:#{pane_zoomed_flag}:#{pane_floating_flag}",
        ]);
        p.run(&[
            "display",
            "-pt",
            "main:0",
            "#{window_layout}|#{window_visible_layout}",
        ]);
    }
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn shrinking_windows_clamps_floating_panes_with_their_borders() {
    let mut p = Pair::new();
    for args in [
        vec![
            "newp",
            "-t",
            "%0",
            "-x",
            "60",
            "-y",
            "20",
            "-X",
            "40",
            "-Y",
            "10",
            "sleep 120",
        ],
        vec![
            "newp",
            "-B",
            "none",
            "-t",
            "%0",
            "-x",
            "35",
            "-y",
            "15",
            "-X",
            "-5",
            "-Y",
            "-4",
            "sleep 120",
        ],
        vec!["resizew", "-t", "main:0", "-x", "30", "-y", "12"],
        vec!["resizew", "-t", "main:0", "-x", "90", "-y", "40"],
        vec!["killp", "-t", "%0"],
        vec!["killp", "-t", "%2"],
        vec!["resizew", "-t", "main:0", "-x", "20", "-y", "8"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    }
}

#[test]
#[ignore = "requires current C tmux floating zoom and PTY/socket access"]
fn over_zoom_stacking_skips_hidden_floats() {
    let mut p = Pair::new();
    for args in [
        vec!["splitw", "-d", "-t", "%0"],
        vec!["newp", "-Ad", "-t", "%0", "sleep 120"],
        vec!["newp", "-d", "-t", "%0", "sleep 120"],
        vec!["newp", "-Ad", "-t", "%0", "sleep 120"],
        vec!["resizep", "-Z", "-t", "%0"],
        vec!["movep", "-t", "%4", "-P", "back"],
        vec!["movep", "-t", "%4", "-P", "front"],
        vec!["movep", "-t", "%4", "-P", "backward-loop"],
        vec!["movep", "-t", "%4", "-z", "99"],
        vec!["resizep", "-Z", "-t", "%0"],
    ] {
        p.run(&args);
        p.run(&["list-panes", "-a", "-F", "#{pane_id}:#{pane_z}"]);
        p.run(&[
            "display",
            "-pt",
            "main:0",
            "#{window_layout}|#{window_visible_layout}",
        ]);
    }
}

#[test]
#[ignore = "requires C tmux pane border status geometry and PTY/socket access"]
fn pane_border_status_reserves_only_outer_tiled_rows() {
    let mut p = Pair::new();
    for args in [
        vec!["set", "-w", "pane-border-status", "top"],
        vec!["splitw", "-h", "-t", "%0"],
        vec!["splitw", "-v", "-l", "50%", "-t", "%0"],
        vec!["set", "-w", "pane-border-status", "bottom"],
        vec!["set", "-p", "-t", "%0", "pane-border-status", "top"],
        vec!["resizep", "-t", "%1", "-Z"],
        vec!["resizep", "-t", "%1", "-Z"],
        vec!["newp", "-t", "%0", "-B", "none", "sleep 120"],
        vec!["set", "-w", "pane-border-status", "top-floating"],
        vec!["set", "-wu", "pane-border-status"],
        vec!["set", "-gw", "pane-border-status", "top"],
        vec!["killp", "-t", "%2"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&["display", "-pt", "main:0", "#{window_layout}"]);
    }
}

#[test]
#[ignore = "requires current C tmux floating layouts and PTY/socket access"]
fn floating_ownership_and_saved_geometry_follow_moves_and_swaps() {
    let mut p = Pair::new();
    for args in [
        vec![
            "new-pane", "-A", "-d", "-t", "%0", "-x", "27", "-y", "9", "-X", "4", "-Y", "3", "",
        ],
        vec!["splitw", "-h", "-t", "%0", ""],
        vec!["joinp", "-s", "%1", "-t", "%1"],
        vec!["swapp", "-s", "%1", "-t", "%2"],
        vec!["breakp", "-W", "-s", "%2"],
        vec!["resizep", "-Z", "-t", "%0"],
        vec!["resizep", "-Z", "-t", "%0"],
        vec!["breakp", "-d", "-s", "%1", "-t", "main:1", "-n", "moved"],
        vec!["splitw", "-h", "-t", "%1", ""],
        vec!["breakp", "-W", "-s", "%1"],
        vec!["resizep", "-Z", "-t", "%3"],
        vec!["resizep", "-Z", "-t", "%3"],
        vec!["joinp", "-s", "%1", "-t", "%0"],
        vec!["breakp", "-W", "-s", "%1"],
        vec!["swapp", "-s", "%1", "-t", "%3"],
        vec!["rotatew", "-t", "main:0"],
    ] {
        p.run(&args);
        state(&mut p);
        p.run(&[
            "list-windows",
            "-a",
            "-F",
            "#{window_id}|#{window_layout}|#{window_visible_layout}",
        ]);
    }
}

#[test]
#[ignore = "requires C tmux"]
fn select_pane_last_title_input_styles_and_zoom_flags_match_c() {
    let mut pair = common::Pair::new();
    pair.run(&["selectp", "-t", "%0", "-T", "initial"]);
    for args in [
        vec!["splitw", "-d", "sleep 120"],
        vec!["last-pane", "-d"],
        vec!["last-pane", "-e"],
        vec!["last-pane"],
        vec!["selectp", "-t", "%0", "-T", "title-#{pane_id}"],
        vec!["selectp", "-t", "%0", "-P", "fg=red,bg=blue", "-g"],
        vec!["selectp", "-t", "%0", "-g"],
        vec!["resizep", "-Z", "-t", "%1"],
        vec!["selectp", "-Z", "-t", "%0"],
        vec!["selectp", "-Z", "-D"],
        vec!["selectp", "-t", "%0"],
    ] {
        pair.run(&args);
        pair.run(&["list-panes", "-F", "#{pane_id}:#{pane_active}:#{pane_last}:#{pane_input_off}:#{pane_left},#{pane_top},#{pane_width},#{pane_height}:#{window_zoomed_flag}"]);
        pair.run(&[
            "display",
            "-p",
            "-t",
            "%0",
            "#{pane_title}|#{window-style}|#{window-active-style}",
        ]);
    }
}
