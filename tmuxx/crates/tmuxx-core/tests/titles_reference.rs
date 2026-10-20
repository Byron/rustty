mod common;
use common::Pair;

#[test]
#[ignore = "requires separate C tmux oracle and socket access"]
fn title_and_legacy_rename_options_control_real_input() {
    for chunk in [1, 7, usize::MAX] {
        let mut pair = Pair::new();
        pair.run(&["new-window", "-d", "-t", "main:1", "-n", "empty", ""]);
        pair.run(&["select-pane", "-t", "%1", "-T", "initial"]);
        let inspect = [
            "display-message",
            "-p",
            "-t",
            "%1",
            "#{pane_title}|#{window_name}|#{automatic-rename}",
        ];
        for (commands, bytes) in [
            (vec![], b"\x1bkblocked\x1b\\".as_slice()),
            (vec![], b"\x1b]2;first title\x07"),
            (
                vec![vec![
                    "set-option",
                    "-w",
                    "-t",
                    "main:1",
                    "allow-set-title",
                    "off",
                ]],
                b"\x1b]0;blocked\x07\x1b_blocked too\x1b\\",
            ),
            (
                vec![vec!["select-pane", "-t", "%1", "-T", "explicit\ntitle\\"]],
                b"\x1b]2;still blocked\x07",
            ),
            (
                vec![vec![
                    "set-option",
                    "-p",
                    "-t",
                    "%1",
                    "allow-set-title",
                    "on",
                ]],
                b"\x1b_apc #(job) \\ title\x1b\\",
            ),
            (vec![], b"\x1bXignored SOS\x1b\\\x1b^ignored PM\x1b\\"),
            (vec![], b"\x1b]2;invalid \xff\x07"),
            (vec![], b"\x1b]2;cancelled\x18\x1b_cancelled\x1a"),
            (vec![], b"\x1b]2\x07"),
            (
                vec![vec!["set-option", "-p", "-t", "%1", "allow-rename", "on"]],
                b"\x1bkrenamed #(job) \\ name\x1b\\",
            ),
            (vec![], b"\x1bkbad \xff\x1b\\"),
            (vec![], b"\x1bkcancelled\x18"),
            (
                vec![vec!["set-option", "-gw", "automatic-rename", "off"]],
                b"\x1bk\x1b\\",
            ),
            (
                vec![
                    vec!["set-option", "-p", "-t", "%1", "allow-rename", "off"],
                    vec!["set-option", "-w", "-t", "main:1", "allow-rename", "on"],
                ],
                b"\x1bkblocked by pane\x1b\\",
            ),
            (
                vec![vec!["set-option", "-pu", "-t", "%1", "allow-rename"]],
                b"\x1bk25a1;plain name\x1b\\",
            ),
        ] {
            for command in commands {
                pair.run(&command);
            }
            pair.feed("%1", 1, bytes, chunk);
            pair.run(&inspect);
        }
        let mut title = b"\x1b]2;".to_vec();
        title.extend(vec![b'x'; 1100]);
        title.push(7);
        pair.feed("%1", 1, &title, chunk);
        pair.run(&inspect);
    }
}

#[test]
#[ignore = "requires current C tmux"]
fn respawn_retains_the_pane_title() {
    let mut pair = common::Pair::new();
    pair.run(&["select-pane", "-T", "retained title"]);
    for command in ["respawn-pane", "respawn-window"] {
        pair.run(&[command, "-k", "sleep 120"]);
        pair.run(&["display-message", "-p", "#{pane_title}"]);
    }
}
