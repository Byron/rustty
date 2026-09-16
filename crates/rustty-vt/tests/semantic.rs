use rustty_vt::screen::{ClickMotion, PromptRedraw, SemanticClick};
use rustty_vt::{Effect, SemanticContent, Terminal, snapshot};

#[test]
fn semantic_fresh_lines_respect_the_cursor_and_left_margin() {
    for action in *b"LAN" {
        let mut terminal = Terminal::new(8, 5, 0);
        terminal.feed(b"\x1b[?69h\x1b[2;6s\x1b[3;4H\x1b]133;");
        terminal.feed(&[action, 7]);
        assert_eq!(terminal.screen().cursor.col, 1);
        assert_eq!(terminal.screen().cursor.row, 3);
        terminal.feed(b"\x1b]133;");
        terminal.feed(&[action, 7]);
        assert_eq!(terminal.screen().cursor.row, 3);
        assert_eq!(
            terminal.screen().cursor.semantic,
            if action == b'L' {
                SemanticContent::Output
            } else {
                SemanticContent::Prompt
            }
        );
    }
    let mut terminal = Terminal::new(8, 5, 0);
    terminal.feed(b"abc\x1b]133;P;k=s\x07");
    assert_eq!(terminal.screen().cursor.col, 3);
    assert_eq!(terminal.screen().cursor.row, 0);
    assert_eq!(terminal.screen().row(0).semantic, SemanticContent::Input);
    assert_eq!(terminal.screen().cursor.semantic, SemanticContent::Prompt);
}

#[test]
fn input_terminated_by_eol_survives_wrap_snapshot_and_screen_copy() {
    let mut terminal = Terminal::new(3, 3, 0);
    terminal.feed(b"\x1b]133;P\x07\x1b]133;I\x07abcd");
    assert_eq!(
        terminal.screen().row(1).cells[0].semantic(),
        SemanticContent::Input
    );
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
    terminal.feed(b"\r\nX");
    assert_eq!(
        terminal.screen().row(2).cells[0].semantic(),
        SemanticContent::Output
    );

    for switch in [b"\x1b[?47h".as_slice(), b"\x1b[?1049h"] {
        let mut terminal = Terminal::new(8, 3, 0);
        terminal.feed(b"\x1b]133;I\x07");
        terminal.feed(switch);
        terminal.feed(b"A\r\nB");
        assert_eq!(
            terminal.screen().row(0).cells[0].semantic(),
            SemanticContent::Input
        );
        assert_eq!(
            terminal.screen().row(1).cells[0].semantic(),
            SemanticContent::Output
        );
    }
}

#[test]
fn output_markers_reset_semantics_independently_of_host_events() {
    for enabled in [false, true] {
        let mut terminal = Terminal::new(8, 3, 0);
        terminal.shell_command_events = enabled;
        terminal.feed(b"\x1b]133;P\x07prompt\r\n");
        assert_eq!(terminal.screen().row(1).semantic, SemanticContent::Input);
        let effects = terminal.feed(b"\x1b]133;C\x07A");
        assert_eq!(
            effects,
            if enabled {
                vec![Effect::CommandStart]
            } else {
                vec![]
            }
        );
        assert_eq!(terminal.screen().row(1).semantic, SemanticContent::Output);
        terminal.feed(b"\x1b]133;B\x07B\x1b]133;D;2\x07C");
        assert_eq!(
            terminal.screen().row(1).cells[1].semantic(),
            SemanticContent::Input
        );
        assert_eq!(
            terminal.screen().row(1).cells[2].semantic(),
            SemanticContent::Output
        );
    }
}

#[test]
fn semantic_commands_reject_invalid_actions_and_capture_overflow() {
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.feed(b"x\x1b]133;L;\x07\x1b]133;Pextra\x07");
    assert_eq!(terminal.screen().cursor.row, 0);
    assert_eq!(terminal.screen().cursor.semantic, SemanticContent::Output);
    let mut command = b"\x1b]133;A;".to_vec();
    command.extend(std::iter::repeat_n(b'x', 2047));
    command.push(7);
    terminal.feed(&command);
    assert_eq!(terminal.screen().cursor.row, 0);
    assert_eq!(terminal.screen().cursor.semantic, SemanticContent::Output);
}

#[test]
fn prompt_options_keep_native_priority_and_snapshot_click_modes() {
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.feed(b"\x1b]133;A;redraw=last;click_events=2;cl=v;k=s\x07");
    assert_eq!(terminal.shell_prompt_redraw(), PromptRedraw::Last);
    assert_eq!(
        terminal.screen().semantic_click(),
        SemanticClick::Events { relative: true }
    );
    assert_eq!(terminal.screen().row(0).semantic, SemanticContent::Input);
    terminal.feed(b"\x1b]133;N;redraw=bad;redraw=1;click_events=0;cl=w;cl=line\x07");
    assert_eq!(terminal.shell_prompt_redraw(), PromptRedraw::Last);
    assert_eq!(
        terminal.screen().semantic_click(),
        SemanticClick::CursorKeys {
            motion: ClickMotion::SmartVertical
        }
    );
    terminal.feed(b"\x1b]133;I\x07");
    assert!(terminal.screen().input_clears_at_eol());
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
    assert_eq!(terminal.shell_prompt_redraw(), PromptRedraw::Last);
    assert_eq!(
        terminal.screen().semantic_click(),
        SemanticClick::CursorKeys {
            motion: ClickMotion::SmartVertical
        }
    );
    assert!(terminal.screen().input_clears_at_eol());
    terminal.feed(b"\x1b]133;P;redraw=0;click_events=1;k=r\x07");
    assert_eq!(terminal.shell_prompt_redraw(), PromptRedraw::Last);
    assert_eq!(
        terminal.screen().semantic_click(),
        SemanticClick::CursorKeys {
            motion: ClickMotion::SmartVertical
        }
    );
    assert_eq!(terminal.screen().row(0).semantic, SemanticContent::Prompt);
    assert!(!terminal.screen().input_clears_at_eol());
}

#[test]
fn whole_row_erasure_clears_prompt_markers_only_when_unprotected() {
    let mut terminal = Terminal::new(4, 3, 0);
    terminal.feed(b"\x1b[?47h\x1b]133;P\x07abcde\x1b[2;1H\x1b[2K");
    assert_eq!(terminal.screen().row(1).semantic, SemanticContent::Input);
    terminal.feed(b"\x1b[?2J");
    assert!(terminal.screen().row(0).wrapped);
    assert!(terminal.screen().row(1).wrap_continuation);
    assert_eq!(terminal.screen().row(0).semantic, SemanticContent::Prompt);
    terminal.feed(b"\x1b[2J");
    assert!(terminal.screen().rows().all(|row| {
        row.semantic == SemanticContent::Output && !row.wrapped && !row.wrap_continuation
    }));
}

#[test]
fn complete_display_erasure_scrolls_a_bottom_prompt_into_history() {
    let mut terminal = Terminal::new(4, 3, 100);
    terminal.feed(b"\x1b]133;P\x07one\r\ntwo\r\nthree\x1b[2;2H\x1b[2J");
    assert_eq!(terminal.screen().cursor.row, 0);
    assert_eq!(terminal.screen().cursor.col, 0);
    assert!(!terminal.screen().history().next().is_none());
    assert!(terminal.screen().rows().all(|row| {
        row.semantic == SemanticContent::Output
            && row.cells.iter().all(|cell| cell.codepoint().is_none())
    }));
}
