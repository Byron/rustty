//! C shell markers are retained data, independent of tmux's copy-mode widgets.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use serde_json::{Value, json};
#[cfg(unix)]
use std::{path::PathBuf, process::Command};
use tmuxx_terminal::{Effect, Terminal};
fn check(cases: Vec<Value>) {
    #[cfg(windows)]
    let observations: Vec<Vec<Value>> = {
        let remote = remote_reference::RemoteReference::new();
        let output = remote.observe("shell_output", &serde_json::to_vec(&cases).unwrap());
        assert!(
            output.status.success(),
            "remote C shell observation failed: {output:?}"
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    #[cfg(unix)]
    let (observations, dir): (Vec<Vec<Value>>, PathBuf) = {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_owned();
        let dir = PathBuf::from(format!("/tmp/txshell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("fixture.json");
        let result = dir.join("result.json");
        std::fs::write(&fixture, serde_json::to_vec(&cases).unwrap()).unwrap();
        assert!(
            Command::new("python3")
                .arg("-B")
                .arg(root.join("tmuxx/conformance/shell_output.py"))
                .arg(std::env::var_os("TMUXX_C_TMUX").expect("TMUXX_C_TMUX"))
                .arg(&fixture)
                .arg(&result)
                .status()
                .unwrap()
                .success()
        );
        (
            serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap(),
            dir,
        )
    };
    for (case, observations) in cases.iter().zip(observations) {
        let mut terminal = Terminal::new(
            case["cols"].as_u64().unwrap() as u16,
            case["rows"].as_u64().unwrap() as u16,
            case["history"].as_u64().unwrap_or(2000) as usize,
        );
        for (index, (step, expected)) in case["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(observations)
            .enumerate()
        {
            if let Some(bytes) = step["feed"].as_array() {
                terminal.feed(
                    &bytes
                        .iter()
                        .map(|b| b.as_u64().unwrap() as u8)
                        .collect::<Vec<_>>(),
                );
            }
            if let Some(cols) = step["resize"].as_u64() {
                terminal.resize(cols as u16, terminal.size().1);
            }
            let snapshot = terminal.snapshot();
            let screen = &snapshot.primary;
            let markers=screen.all_rows().enumerate().filter_map(|(row,r)|screen.tmux_shell_markers()?.get(&r.id).map(|m|json!({"row":row,"prompt":m.prompt,"command":m.command,"start":m.start,"end":m.end}))).collect::<Vec<_>>();
            assert_eq!(
                json!(markers),
                expected["markers"],
                "{} step{index} retained markers",
                case["name"]
            );
            let point = if let Some(search) = case["search"].as_str() {
                screen
                    .search_literal(search.as_bytes())
                    .first()
                    .expect("search fixture")
                    .start
            } else {
                rustty_vt::GridPoint {
                    row: screen.row(screen.cursor.row).id,
                    col: screen.cursor.col,
                }
            };
            let output = screen.tmux_output_text(point);
            let bytes = expected["output"].as_array().map(|v| {
                v.iter()
                    .map(|b| b.as_u64().unwrap() as u8)
                    .collect::<Vec<_>>()
            });
            assert_eq!(
                output.as_deref().map(str::as_bytes),
                bytes.as_deref(),
                "{} step{index} output; Rust rows {:?}; markers {:?}; C rows {:?}",
                case["name"],
                screen
                    .all_rows()
                    .map(|r| (r.id, r.wrapped, screen.row_text(r)))
                    .collect::<Vec<_>>(),
                screen.tmux_shell_markers(),
                expected["raw"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .filter(|s| s.starts_with("\tL"))
                    .collect::<Vec<_>>()
            );
            let mut restored = terminal.snapshot().restore().unwrap().into_backend();
            assert_eq!(
                restored.screen().tmux_output_text(point),
                output,
                "snapshot restore"
            );
            let selection = restored.screen().select_output(point);
            restored.screen_mut().selection = selection;
            let selected = restored.screen().selection_text();
            let bytes = expected["selection"].as_array().map(|v| {
                v.iter()
                    .map(|b| b.as_u64().unwrap() as u8)
                    .collect::<Vec<_>>()
            });
            assert_eq!(
                selected.as_deref().map(str::as_bytes),
                bytes.as_deref(),
                "{} step{index} selection",
                case["name"]
            );
        }
    }
    #[cfg(unix)]
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
#[ignore = "requires isolated C tmux and TMUXX_C_TMUX"]
fn osc133_output_ranges_same_line_reflow_and_lost_history_match_c() {
    let mut cases = vec![];
    for (name,feed,widths,search) in [
        ("same-line",b"xx\x1b]133;A\x07p>\x1b]133;B\x07echo\x1b]133;C\x07output\x1b]133;D;9\x07tail".to_vec(),vec![8,10,14,80],"echo"),
        ("wrapped",[b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07".as_slice(),&[b'x';120],b"\x1b]133;D;7\x07\r\n\x1b]133;A\x07p>\x1b]133;B\x07"].concat(),vec![40,100,160,40,80],"echo"),
        ("unfinished",b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07first\r\n\r\nlast\x1b[2A\rXY".to_vec(),vec![10,80],"echo"),
        ("prompt-overwrite", b"\x1b]133;P;k=sX;k=s\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07out\x1b]133;D;0\x07\r\n\x1b]133;P;k=s\x07\x1b]133;N\x07p>".to_vec(),vec![4,8,80],"echo"),
        ("secondary", b"\x1b]133;A\x07p>\x1b]133;P;k=s\x07\x1b]133;I\x07echo\r\n\x1b]133;P;k=s\x07..\r\n\x1b]133;C\x07out\x1b]133;D;0\x07\r\n\x1b]133;N\x07p>".to_vec(),vec![4,8,80],"echo"),
        ("wide", "\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07漢字ab 漢\x1b]133;D;0\x07tail".as_bytes().to_vec(),vec![4,7,10,80],"echo"),
        ("gap", b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07one\x1b[5Ctwo\x1b]133;D;0\x07".to_vec(),vec![8,16,80],"echo"),
        ("marker-only", b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b[10C\x1b]133;C\x07\r\nnext\x1b]133;D;0\x07".to_vec(),vec![8,16,80],"echo"),
        ("newline",b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07one   \r\n\x1b]133;D;3\x07".to_vec(),vec![4,16,80],"echo"),
    ] {
        let mut steps=vec![json!({"feed":feed})];steps.extend(widths.into_iter().map(|w|json!({"resize":w})));
        cases.push(json!({"name":name,"cols":80,"rows":20,"search":search,"steps":steps}));
    }
    for complete in [false, true] {
        let feed = [
            b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n".to_vec(),
            b"continued command\r\n".repeat(30),
            b"zz\x1b]133;C\x07one\r\ntwo".to_vec(),
            if complete {
                b"\x1b]133;D;0\x07separator".to_vec()
            } else {
                vec![]
            },
        ]
        .concat();
        cases.push(json!({"name":format!("lost-prompt-{complete}"),"cols":40,"rows":5,"history":3,"search":"one","steps":[{"feed":feed}]}));
    }

    for (name, feed) in [
        ("erase-display", b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07old\x1b[2J\x1b[Hnew\x1b]133;D;0\x07".to_vec()),
        ("erase-line", b"\x1b]133;A\x07p>\x1b]133;B\x07echo\x1b]133;C\x07old\x1b[2K\rnew\x1b]133;D;0\x07".to_vec()),
        ("reset", b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07old\x1bcnew\x1b]133;D;0\x07".to_vec()),
        ("previous-output", b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C\x07old\x1b]133;D;0\x07\r\n\x1b]133;A\x07p>\x1b]133;B\x07next".to_vec()),
    ] {
        cases.push(json!({"name":name,"cols":40,"rows":5,"steps":[{"feed":feed},{"resize":8},{"resize":40}]}));
    }
    check(cases);
}
#[test]
fn osc133_owner_effects_and_invalid_snapshot_markers_are_explicit() {
    let mut terminal = Terminal::new(40, 5, 100);
    assert!(
        terminal
            .feed(b"\x1b]133;N\x07")
            .iter()
            .any(|e| matches!(e, Effect::ShellPrompt))
    );
    assert!(
        terminal
            .feed(b"\x1b]133;C;cmdline_url=echo%20hello\x07")
            .iter()
            .any(
                |e| matches!(e,Effect::TmuxCommandStart{command_line:Some(s)} if s==b"echo hello")
            )
    );
    for (data, status) in [
        ("D", 0),
        ("D;", 0),
        ("D;foo=2", 0),
        ("D;-1", 255),
        ("D;256", 255),
        ("D;7;ignored", 7),
    ] {
        assert!(
            terminal
                .feed(format!("\x1b]133;{data}\x07").as_bytes())
                .iter()
                .any(|e| matches!(e,Effect::CommandEnd{exit_code:Some(s)} if *s==status))
        );
    }
    for data in [
        "C;cmdline_url=%00",
        "C;cmdline_url=%z0",
        "C;cmdline_url=%ff",
    ] {
        assert!(
            terminal
                .feed(format!("\x1b]133;{data}\x07").as_bytes())
                .iter()
                .any(|e| matches!(e, Effect::TmuxCommandStart { command_line: None }))
        );
    }
    let snapshot = terminal.snapshot();
    let row = snapshot.primary.row(0).id;
    let decoded: tmuxx_terminal::Snapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    decoded.restore().unwrap();
    for (key, markers) in [
        (u64::MAX, json!({"start":0})),
        (row, json!({"end":[65536,0]})),
        (row, json!({"prompt":[0,4]})),
    ] {
        let mut encoded = serde_json::to_value(&snapshot).unwrap();
        encoded["primary"]["tmux_shell"] = json!([[key, markers]]);
        let invalid: tmuxx_terminal::Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&encoded).unwrap()).unwrap();
        assert!(
            invalid.restore().is_err(),
            "untrusted shell markers must be rejected"
        );
    }
}

#[test]
fn osc133_snapshot_resumes_every_byte_boundary() {
    let bytes=b"\x1b]133;A\x07p>\x1b]133;B\x07echo\r\n\x1b]133;C;cmdline_url=echo%20one\x1b\\one\r\ntwo\x1b]133;D;7\x07";
    let mut reference = Terminal::new(20, 5, 10);
    reference.feed(bytes);
    let expected = reference.snapshot();
    for split in 0..=bytes.len() {
        let mut actual = Terminal::new(20, 5, 10);
        actual.feed(&bytes[..split]);
        let wire = serde_json::to_vec(&actual.snapshot()).unwrap();
        let saved: tmuxx_terminal::Snapshot = serde_json::from_slice(&wire).unwrap();
        let mut restored = saved.restore().unwrap();
        restored.feed(&bytes[split..]);
        let actual = restored.snapshot();
        assert_eq!(
            serde_json::to_value(actual.primary.tmux_shell_markers()).unwrap(),
            serde_json::to_value(expected.primary.tmux_shell_markers()).unwrap(),
            "byte {split}"
        );
        let point = rustty_vt::GridPoint {
            row: actual.primary.row(actual.primary.cursor.row).id,
            col: actual.primary.cursor.col,
        };
        assert_eq!(
            actual.primary.tmux_output_text(point).as_deref(),
            Some("one\ntwo"),
            "byte {split}"
        );
    }
}
