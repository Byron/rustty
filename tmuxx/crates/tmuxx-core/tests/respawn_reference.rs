mod common;
use common::Pair;

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn respawn_retains_detached_cell_pixel_defaults() {
    let mut pair = Pair::new();
    for command in ["respawn-pane", "respawn-window"] {
        pair.run(&[
            "display-message",
            "-p",
            "#{window_cell_width}|#{window_cell_height}",
        ]);
        pair.run(&[command, "-kE"]);
        pair.run(&[
            "display-message",
            "-p",
            "#{window_cell_width}|#{window_cell_height}",
        ]);
    }
}
