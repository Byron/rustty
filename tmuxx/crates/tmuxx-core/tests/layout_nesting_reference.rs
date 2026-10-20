mod common;
use common::Pair;

#[test]
#[ignore = "requires current C tmux and PTY/socket access"]
fn legacy_unary_depth_boundary_matches_c_geometry_and_json() {
    let mut pair = Pair::new();
    for (open, close) in [('{', '}'), ('[', ']')] {
        let body = format!(
            "{}1x1,0,0{}",
            format!("1x1,0,0{open}").repeat(1000),
            close.to_string().repeat(1000)
        );
        let wire = format!("{:04x},{body}", tmuxx_layout::checksum(body.as_bytes()));
        pair.run(&["select-layout", &wire]);
        pair.run(&[
            "display-message",
            "-p",
            "#{pane_id}|#{pane_width}|#{pane_height}|#{window_layout}",
        ]);
    }
    pair.run(&["select-layout", r#"{"V":2,"L":{"t":"h","w":1,"h":1,"x":0,"y":0,"c":[{"t":"p","w":1,"h":1,"x":0,"y":0,"i":0}]}}"#]);
}
