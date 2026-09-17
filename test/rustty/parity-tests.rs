use super::*;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

struct Temporary(PathBuf);

impl Temporary {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "rustty-parity-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("{error}"),
            }
        }
    }

    fn peer(&self, name: &'static str, script: &str, timeout: Duration) -> Peer {
        Peer::spawn(
            name,
            Command::new("/bin/sh").args(["-c", script]),
            &self.0,
            timeout,
        )
        .unwrap()
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn differences_are_strict_and_exclusions_only_apply_at_the_root() {
    let left = json!({"cells":[{"text":[65],"color":[1,2,3]}],"events":["bell"]});
    let mut right = left.clone();
    right["cells"][0]["color"][1] = json!(9);
    assert!(
        difference(&left, &right, "response", &[])
            .unwrap()
            .contains("color[1]")
    );
    right["cells"] = left["cells"].clone();
    right["events"] = json!([]);
    assert!(
        difference(&left, &right, "response", &[])
            .unwrap()
            .contains("events")
    );
    assert!(difference(&left, &left, "response", &[]).is_none());
    for (left, right) in [
        (json!(true), json!(1)),
        (json!(1), json!(1.0)),
        (json!({"field":null}), json!({})),
    ] {
        assert!(difference(&left, &right, "response", &[]).is_some());
    }
    let mut left = json!({"capabilities":[1],"state":{"capabilities":[1]}});
    let right = json!({"capabilities":[2],"state":{"capabilities":[1]}});
    assert!(difference(&left, &right, "response", &["capabilities"]).is_none());
    left["state"]["capabilities"] = json!([2]);
    assert!(
        difference(&left, &right, "response", &["capabilities"])
            .unwrap()
            .contains("state.capabilities")
    );
}

fn collapse(operations: &Value) -> Value {
    let mut result = Vec::new();
    let mut pending = Vec::new();
    for operation in operations.as_array().unwrap() {
        if operation["op"] == "write" {
            pending.extend(unhex(operation["data"].as_str().unwrap()).unwrap());
        } else {
            result.extend([json!(hex(&pending)), operation.clone()]);
            pending.clear();
        }
    }
    result.push(json!(hex(&pending)));
    json!(result)
}

#[test]
fn delivery_keeps_bytes_barriers_and_bounded_transport() {
    let data = hex("aé界\x1b[31m".as_bytes());
    let request = json!({"id":"delivery","kind":"snapshot","operations":[
        {"op":"write","data":data},{"op":"observe"},{"op":"resize","cols":5,"rows":2},
        {"op":"write","data":"ff"},{"op":"osc","data":"ff0732"}],
        "after":[{"op":"write","data":data},{"op":"reset"}]});
    let mut count = 0;
    each_variant(&request, true, false, |variant| {
        for field in ["operations", "after"] {
            assert_eq!(collapse(&variant[field]), collapse(&request[field]));
        }
        if count == 1 {
            assert_eq!(variant["scalar"], true);
            assert_eq!(variant["operations"], request["operations"]);
        }
        count += 1;
        Ok(true)
    })
    .unwrap();
    assert!(count > 3);
    let data: Vec<_> = (0..=255).cycle().take(131072).collect();
    let request = json!({"id":"large","operations":[{"op":"write","data":hex(&data)}]});
    each_variant(&request, false, false, |variant| {
        assert_eq!(collapse(&variant["operations"]), json!([hex(&data)]));
        assert!(serde_json::to_vec(variant)?.len() < data.len() * 2 + 65536);
        Ok(true)
    })
    .unwrap();
    assert_eq!(unhex(" 00\tFf\n80").unwrap(), [0, 255, 128]);
    for invalid in ["a b", "f", "gg"] {
        assert!(unhex(invalid).is_err());
    }
    for request in [
        json!({"id":"rejection","expected_error":"InvalidSnapshot"}),
        json!({"id":"unicode","kind":"unicode"}),
    ] {
        let mut count = 0;
        each_variant(&request, true, false, |_| {
            count += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn snapshots_check_live_state_and_expected_errors() {
    let request = json!({"id":"roundtrip","kind":"snapshot","operations":[],"after":[]});
    for (loses_state, fails) in [(false, false), (true, false), (false, true)] {
        let result = compare(&request, |index, request| {
            let has = |name| request["operations"].as_array().unwrap().iter().any(|op| op["op"] == name);
            let encoding = has("snapshot");
            let failed = fails && !encoding;
            let state = if failed { json!([]) } else if encoding { json!(["A"]) }
                else if loses_state && has("restore") { json!(["", "B"]) } else { json!(["A", "AB"]) };
            Ok(json!({"id":request["id"],"ok":!failed,"err":if failed {json!("InvalidSnapshot")} else {Value::Null},
                "observations":state,"snapshots":if encoding {json!([index.to_string()])} else {json!([])}}))
        }).unwrap();
        assert_eq!(result.reason.is_some(), loses_state || fails);
        if loses_state {
            assert!(result.reason.unwrap().contains("restore-zig"));
        }
    }
    let request = json!({"id":"bad-wire","expected_error":"InvalidSnapshot","operations":[]});
    for error in [Some("InvalidSnapshot"), Some("UnsupportedOperation"), None] {
        let result = compare(&request, |_, request| {
            assert!(request.get("expected_error").is_none());
            Ok(json!({"id":request["id"],"ok":error.is_none(),"err":error}))
        })
        .unwrap();
        assert_eq!(result.reason.is_none(), error == Some("InvalidSnapshot"));
    }
}

#[test]
fn coverage_cannot_pass_without_capabilities_complete_status_and_passing_cases() {
    let mut manifest =
        json!({"requirements":[{"id":"terminal.cells","status":"partial","remaining":"controls"}]});
    let covered = BTreeSet::from(["terminal.cells".into()]);
    let mut capabilities = [covered.clone(), covered.clone()];
    assert!(
        !coverage_gaps(&manifest, &capabilities, &covered)
            .unwrap()
            .is_empty()
    );
    manifest["requirements"][0]["status"] = json!("complete");
    assert!(
        !coverage_gaps(&manifest, &capabilities, &BTreeSet::new())
            .unwrap()
            .is_empty()
    );
    assert!(
        coverage_gaps(&manifest, &capabilities, &covered)
            .unwrap()
            .is_empty()
    );
    capabilities[0].clear();
    assert!(
        !coverage_gaps(&manifest, &capabilities, &covered)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn bounded_transport_rejects_truncation_oversize_invalid_json_and_oracle_death() {
    let mut buffer = Vec::new();
    for bytes in [&b"{}"[..], &b"xxxxxxxxxxxxxxxx\n"[..], &b"invalid\n"[..]] {
        assert!(read_json::<Value>(&mut &*bytes, &mut buffer, 16).is_err());
    }
    assert_eq!(
        read_json::<Value>(&mut &b"{}\n"[..], &mut buffer, 3).unwrap(),
        Some(json!({}))
    );
    assert_eq!(
        read_json::<Value>(&mut &b""[..], &mut buffer, 3).unwrap(),
        None
    );
    let temporary = Temporary::new();
    for script in ["exit 3", "printf '%s' '{\"ok\":true}'"] {
        let mut peer = temporary.peer("broken", script, Duration::from_secs(2));
        assert!(peer.request(&json!({})).is_err());
        assert!(peer.process.try_wait().unwrap().is_some());
    }
    let mut peer = temporary.peer("request-limit", "exec sleep 30", Duration::from_secs(2));
    assert!(
        peer.request(&json!({"data":"x".repeat(MAX_REQUEST)}))
            .unwrap_err()
            .to_string()
            .contains("request exceeded")
    );
}

#[test]
fn deadlines_cover_full_input_pipes_and_cleanup_children() {
    let temporary = Temporary::new();
    for request in [json!({}), json!({"data":"x".repeat(1024 * 1024)})] {
        let mut peer = temporary.peer("blocked", "exec sleep 30", Duration::from_millis(150));
        let start = Instant::now();
        assert!(
            peer.request(&request)
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(peer.process.try_wait().unwrap().is_some());
    }
    let peer = temporary.peer("cleanup", "exec sleep 30", Duration::from_secs(2));
    let pid = peer.process.id();
    drop(peer);
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn delivery_failures_preserve_external_artifacts_and_do_not_grant_coverage() {
    let temporary = Temporary::new();
    let script = r#"while IFS= read -r line; do
        case "$line" in *'"scalar":true'*) cell=2;; *) cell=1;; esac
        printf '{"ok":true,"err":null,"cell":%s}\n' "$cell"
    done"#;
    let mut options = Options::parse(std::iter::empty()).unwrap().unwrap();
    options.artifacts = temporary.0.clone();
    let mut runner = Runner {
        options,
        peers: [
            temporary.peer("zig", script, Duration::from_secs(2)),
            temporary.peer("rust", script, Duration::from_secs(2)),
        ],
        capabilities: Default::default(),
        covered: BTreeSet::new(),
        checked: 0,
        failures: 0,
        aborted: false,
    };
    let request = json!({"id":"delivery","operations":[{"op":"write","data":"61"}]});
    runner
        .fixture(Fixture {
            request: request.clone(),
            covers: vec!["terminal.cells".into()],
        })
        .unwrap();
    assert_eq!((runner.checked, runner.failures), (3, 1));
    assert!(runner.covered.is_empty());
    let comparison = Comparison {
        values: [json!({"ok":true}), Value::Null],
        reason: None,
    };
    let first = runner.save_failure(&request, &comparison, "first").unwrap();
    let second = runner
        .save_failure(&request, &comparison, "second")
        .unwrap();
    assert!(first.is_absolute());
    assert_ne!(first, second);
    assert_eq!(
        fs::read_to_string(first.join("difference.txt")).unwrap(),
        "first\n"
    );
    assert_eq!(
        serde_json::from_reader::<_, Value>(File::open(second.join("request.json")).unwrap())
            .unwrap(),
        request
    );
}

#[test]
fn minimization_retains_the_mismatch_category_and_attempt_limit() {
    let request = json!({"id":"reduce","operations":[{"op":"observe"},{"op":"write","data":hex(b"abcYZdef")}]});
    let mut send = |index, request: &Value| -> Result<Value> {
        let bytes: Vec<_> = request["operations"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|op| op["data"].as_str())
            .flat_map(|data| unhex(data).unwrap())
            .collect();
        Ok(
            json!({"ok":true,"cells":[u8::from(index == 1 && bytes.contains(&b'Z'))],
            "other":index == 1 && bytes.contains(&b'Y')}),
        )
    };
    let (reduced, result, attempts) = minimize(&request, &mut send).unwrap();
    assert_eq!(reduced["operations"], json!([{"op":"write","data":"5a"}]));
    assert!(result.reason.unwrap().contains("cells"));
    assert!(attempts < 2000);
    let mut attempts = 0;
    let sequence = vec![0; 4096];
    assert_eq!(
        reduce(sequence.clone(), &mut attempts, |_| Ok(false)).unwrap(),
        sequence
    );
    assert_eq!(attempts, 2000);
    assert!(minimize(&request, &mut |_, _| Ok(json!({"ok":true}))).is_err());
}

#[test]
fn cli_rejects_invalid_deadlines_and_normalizes_integer_seeds() {
    for args in [
        vec!["--timeout", "0"],
        vec!["--timeout", "NaN"],
        vec!["--minimize"],
        vec!["--generated", "-1"],
        vec!["--seed", "no"],
        vec!["--max-failures", "0"],
    ] {
        assert!(Options::parse(args.into_iter().map(str::to_owned)).is_err());
    }
    assert_eq!(cases::seed("-00042").unwrap(), "-42");
    assert_eq!(cases::seed("+0000").unwrap(), "0");
}

#[test]
fn archives_reject_stale_inputs_probes_membership_counts_and_truncation() {
    use flate2::{Compression, write::GzEncoder};
    let temporary = Temporary::new();
    let root = &temporary.0;
    fs::create_dir_all(root.join("test/rustty/fixtures")).unwrap();
    fs::create_dir(root.join("corpus")).unwrap();
    fs::write(root.join("corpus/original"), b"x").unwrap();
    fs::write(root.join("smoke.json"), b"[]").unwrap();
    let oracle = root.join("oracle");
    fs::write(&oracle, "#!/bin/sh\nwhile IFS= read -r line; do printf '%s\\n' '{\"ok\":true,\"capabilities\":[]}'; done\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&oracle, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut crc = flate2::Crc::new();
    crc.update(b"original");
    let archive = root.join("test/rustty/fixtures/input.jsonl.gz");
    for failure in ["none", "input", "membership", "probe", "count", "truncated"] {
        fs::write(
            root.join("source"),
            if failure == "input" {
                b"changed".as_slice()
            } else {
                b"original"
            },
        )
        .unwrap();
        if failure == "membership" {
            fs::write(root.join("corpus/new"), b"x").unwrap();
        }
        let header = json!({"format":1,"cases":if failure == "count" {2} else {1},
            "inputs":{"source":crc.sum()},"directories":{"corpus":["original"]},
            "probes":[{"request":{"id":"probe"},"response":{"ok":failure != "probe","capabilities":[]}}]});
        let fixture =
            json!({"request":{"id":"fixture","kind":"unicode","codepoints":[65]},"covers":[]});
        let mut output = GzEncoder::new(Vec::new(), Compression::default());
        for record in [header, fixture] {
            serde_json::to_writer(&mut output, &record).unwrap();
            output.write_all(b"\n").unwrap();
        }
        let mut bytes = output.finish().unwrap();
        if failure == "truncated" {
            bytes.truncate(bytes.len() - 4);
        }
        fs::write(&archive, bytes).unwrap();
        let mut options = Options::parse(std::iter::empty()).unwrap().unwrap();
        options.root = root.clone();
        options.artifacts = root.join("results");
        options.fixtures = root.join("smoke.json");
        options.zig = oracle.clone();
        options.rust = oracle.clone();
        options.no_build = true;
        options.groups.insert("input".into());
        assert_eq!(run(options).unwrap(), failure == "none", "{failure}");
        let summary: Value =
            serde_json::from_reader(File::open(root.join("results/summary.json")).unwrap())
                .unwrap();
        assert_eq!(summary["aborted"], failure != "none");
        assert_eq!(summary["full_parity"], false);
        if failure == "membership" {
            fs::remove_file(root.join("corpus/new")).unwrap();
        }
    }
}
