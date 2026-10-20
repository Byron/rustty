#![cfg(windows)]

#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;

use remote_reference::RemoteReference;
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires pinned real C oracle and native packaged ConPTY"]
fn native_pipe_and_real_c_socket_selection_preserve_independent_instances() {
    let reference = RemoteReference::new();
    let observed = reference.observe("upstream_reference", br#"{"case":"socket_defaults"}"#);
    assert!(observed.status.success(), "{observed:?}");
    let observed: serde_json::Value = serde_json::from_slice(&observed.stdout).unwrap();
    assert_eq!(observed["case"], "socket_defaults");
    assert_eq!(observed["identities"].as_array().unwrap().len(), 4);
    assert_eq!(
        observed["default_label_environment_and_explicit_precedence"],
        true
    );

    let source = r#"
import json,os,sys,tempfile
from pathlib import Path
sys.path.insert(0,sys.argv[2])
from windows_cli_state import CLIState
with tempfile.TemporaryDirectory(prefix='native-discovery-') as temporary:
    candidate=Path(sys.argv[1])
    fixture=CLIState(candidate,candidate,Path(temporary))
    if 'TMUXX_CONPTY_RESOURCES' in os.environ:
        fixture.env['TMUXX_CONPTY_RESOURCES']=os.environ['TMUXX_CONPTY_RESOURCES']
    try:
        fixture.namespace()
        result=fixture.evidence['namespace']
    finally:
        remaining=fixture.cleanup()
        assert not remaining, ('native fixture processes survived',remaining)
    result['cleanup_remaining']=remaining
    print(json.dumps(result))
"#;
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance");
    let python = PathBuf::from(
        std::env::var_os("TMUXX_REMOTE_PYTHON")
            .expect("set TMUXX_REMOTE_PYTHON to the absolute native Python executable"),
    );
    let mut child = tmuxx_runtime::spawn_owned_stdio_piped_stderr(
        &python,
        &[
            "-X".into(),
            "utf8".into(),
            "-c".into(),
            source.into(),
            env!("CARGO_BIN_EXE_tmuxx").into(),
            helper.into_os_string(),
        ],
    )
    .unwrap();
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
    let deadline = Instant::now() + Duration::from_secs(50);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native namespace fixture exceeded bounded wait");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = output.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(
        status.success(),
        "native fixture stdout={output:?}; stderr={errors:?}"
    );
    let native: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(native["cleanup_remaining"], serde_json::json!([]));
    assert_eq!(
        native["fixed_bridge_protocol_selection_matches_local_pipes"],
        true
    );
    assert_ne!(native["incarnations"][0], native["incarnations"][1]);
    eprintln!("TMUXX_NATIVE_DISCOVERY={native}");
}
