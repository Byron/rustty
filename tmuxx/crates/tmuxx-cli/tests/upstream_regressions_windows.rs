//! Upstream files remain unchanged. Native fixtures explicitly start Windows
//! owners; raw-console and pipe counterparts preserve the original vectors.
#![cfg(windows)]

#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;

use remote_reference::RemoteReference;
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

fn native(script: &str) -> (bool, Vec<u8>) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let helper = root.join("tmuxx/conformance/windows_upstream.py");
    let source = std::env::var_os("TMUXX_C_SOURCE")
        .map(|root| PathBuf::from(root).join("regress").join(script));
    let python = PathBuf::from(
        std::env::var_os("TMUXX_REMOTE_PYTHON")
            .expect("set TMUXX_REMOTE_PYTHON to absolute native Python"),
    );
    let mut arguments = vec![
        "-X".into(),
        "utf8".into(),
        helper.into_os_string(),
        "--candidate".into(),
        env!("CARGO_BIN_EXE_tmuxx").into(),
    ];
    if script == "harness-cleanup" {
        arguments.push("--cleanup-regression".into());
    } else {
        arguments.extend([
            "--script".into(),
            source.expect("set TMUXX_C_SOURCE").into_os_string(),
        ]);
    }
    let mut child = tmuxx_runtime::spawn_owned_stdio_piped_stderr(&python, &arguments).unwrap();
    child.stdin.take();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(145);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("upstream native fixture exceeded bounded wait: {script}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = output.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(
        errors.is_empty(),
        "{script}: {}",
        String::from_utf8_lossy(&errors)
    );
    let observed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(status.success(), observed["returncode"] == 0);
    assert_eq!(observed["cleanup_remaining"], serde_json::json!([]));
    use base64::Engine;
    let mut transcript = base64::engine::general_purpose::STANDARD
        .decode(observed["stdout"].as_str().unwrap())
        .unwrap();
    let script_errors = base64::engine::general_purpose::STANDARD
        .decode(observed["stderr"].as_str().unwrap())
        .unwrap();
    if matches!(script, "input-raw-controls.sh" | "input-nowrap-collect.sh") {
        assert!(
            script_errors.is_empty(),
            "raw difference fixture had command errors: {}",
            String::from_utf8_lossy(&script_errors)
        );
    }
    transcript.extend(script_errors);
    eprintln!(
        "TMUXX_NATIVE_UPSTREAM={}",
        String::from_utf8(output).unwrap()
    );
    (status.success(), transcript)
}

#[test]
#[ignore = "requires native Windows ConPTY and Git test shell"]
fn harness_cleans_flooding_owners_after_script_exit_and_timeout() {
    let (success, transcript) = native("harness-cleanup");
    assert!(success && transcript.is_empty());
}

fn reference(reference: &RemoteReference, script: &str) {
    let request = serde_json::json!({"case":script});
    let observed = reference.observe("upstream_reference", request.to_string().as_bytes());
    assert!(observed.status.success(), "{script}: {observed:?}");
    let observed: serde_json::Value = serde_json::from_slice(&observed.stdout).unwrap();
    assert_eq!(observed["case"], script);
    use base64::Engine;
    let transcript = base64::engine::general_purpose::STANDARD
        .decode(observed["transcript"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        observed["returncode"],
        0,
        "original C {script}: {}",
        String::from_utf8_lossy(&transcript)
    );
}

macro_rules! regression {
    ($name:ident,$script:literal) => {
        #[test]
        #[ignore = "requires pinned real C, native Windows ConPTY and Git test shell"]
        fn $name() {
            let oracle = RemoteReference::new();
            reference(&oracle, $script);
            let (success, transcript) = native($script);
            assert!(
                success,
                "native {}: {}",
                $script,
                String::from_utf8_lossy(&transcript)
            );
        }
    };
}

regression!(upstream_input_cursor, "input-cursor.sh");
regression!(upstream_input_edit, "input-edit.sh");
regression!(upstream_input_lines_region, "input-lines-region.sh");
regression!(upstream_input_modes, "input-modes.sh");
regression!(upstream_input_malformed, "input-malformed.sh");
regression!(upstream_input_scroll, "input-scroll.sh");
regression!(upstream_input_sgr, "input-sgr.sh");
regression!(upstream_input_unicode, "input-unicode.sh");
regression!(upstream_input_osc, "input-osc.sh");
regression!(upstream_format_fuzzy, "format-fuzzy.sh");
regression!(upstream_format_modifiers, "format-modifiers.sh");
regression!(
    upstream_format_no_current_window,
    "format-no-current-window.sh"
);
regression!(upstream_format_time_range, "format-time-range.sh");
regression!(upstream_json, "json.sh");
regression!(upstream_buffers, "buffers.sh");
regression!(upstream_set_hook_b, "set-hook-B.sh");
regression!(upstream_targets, "targets.sh");
regression!(upstream_targets_panes, "targets-panes.sh");
regression!(upstream_targets_current, "targets-current.sh");
regression!(upstream_layout_nesting_limit, "layout-nesting-limit.sh");
regression!(upstream_control_client_exit, "control-client-exit.sh");
regression!(
    upstream_control_client_exit_stalled,
    "control-client-exit-stalled.sh"
);
regression!(
    upstream_control_client_run_shell_exit,
    "control-client-run-shell-exit.sh"
);
regression!(
    upstream_control_client_wait_exit,
    "control-client-wait-exit.sh"
);
regression!(upstream_control_notify_guard, "control-notify-guard.sh");
regression!(upstream_control_subscriptions, "control-subscriptions.sh");

#[test]
#[ignore = "requires pinned real C, native Windows ConPTY and Git test shell"]
fn upstream_format_strings_requires_excluded_copy_mode() {
    let oracle = RemoteReference::new();
    reference(&oracle, "format-strings.sh");
    let (success, transcript) = native("format-strings.sh");
    assert!(
        !success,
        "original format-strings unexpectedly passed; review its UI scope"
    );
    // The helper retains stdout and stderr separately; native() joins them in
    // that order without pretending to reconstruct inter-stream scheduling.
    assert_eq!(transcript,b"Format test failed for '#{?pane_in_mode,abc}'.\nExpected: 'abc'\nBut got   ''\nunsupported command: copy-mode\n");
}

#[test]
#[ignore = "requires pinned real C, native Windows ConPTY and Git test shell"]
fn upstream_raw_diagnostic_differences_remain_explicit() {
    let oracle = RemoteReference::new();
    for (script, expected) in [
        (
            "input-raw-controls.sh",
            vec![
                "FAIL: charset raw missing",
                "FAIL: charset raw missing",
                "FAIL: charset raw missing",
                "FAIL: g1charset raw missing",
            ],
        ),
        ("input-nowrap-collect.sh", vec!["FAIL: padding raw missing"]),
    ] {
        reference(&oracle, script);
        let (success, transcript) = native(script);
        assert!(!success, "{script} unexpectedly passed; retire difference");
        let transcript = String::from_utf8(transcript).unwrap();
        assert_eq!(
            transcript
                .lines()
                .filter(|line| line.starts_with("FAIL:"))
                .collect::<Vec<_>>(),
            expected,
            "{script}: {transcript}"
        );
        assert!(
            !transcript.contains("tmuxx:"),
            "unexpected native driver error: {transcript}"
        );
    }
}
